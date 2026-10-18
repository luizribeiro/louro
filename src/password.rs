//! The bot's SASL password, read once at `connect` and never from argv or env.

use std::env;
use std::fs::File;
use std::io::{self, IsTerminal};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use crate::error::{Code, Error, Result};
use crate::irc::Secret;

pub enum Source {
    /// A file only its owner can read; a systemd credential's group bits are let through.
    File(PathBuf),
    /// A pipe from the `connect` that spawned this daemon.
    Stdin,
    /// A no-echo prompt for this SASL account.
    Tty(String),
}

pub fn read(source: &Source) -> Result<Secret> {
    let mut password = match source {
        Source::File(path) => {
            let credentials_dir = env::var_os("CREDENTIALS_DIRECTORY");
            from_file(path, credentials_dir.as_deref().map(Path::new))?
        }
        Source::Stdin => io::read_to_string(io::stdin()).map_err(unreadable)?,
        Source::Tty(user) => from_tty(user)?,
    };
    password.truncate(password.trim_end_matches(['\r', '\n']).len());
    Ok(Secret::new(password))
}

fn from_file(path: &Path, credentials_dir: Option<&Path>) -> Result<String> {
    let file = File::open(path).map_err(unreadable)?;
    let mode = file.metadata().map_err(unreadable)?.permissions().mode();
    // systemd lets only the service read a credential, through an ACL that shows in the group bits.
    let exposed = if credentials_dir.is_some_and(|dir| path.parent() == Some(dir)) {
        0o007
    } else {
        0o077
    };
    if mode & exposed != 0 {
        return Err(Error::new(
            Code::Usage,
            format!("{} is readable by other users", path.display()),
        )
        .with_hint("chmod 600 it"));
    }
    io::read_to_string(file).map_err(unreadable)
}

fn from_tty(user: &str) -> Result<String> {
    if !io::stdin().is_terminal() {
        return Err(Error::new(
            Code::Usage,
            "there is no terminal to ask for the password on",
        )
        .with_hint(
            "run `louro connect` in your own terminal, or set password_file in louro.toml",
        ));
    }
    rpassword::prompt_password(format!("SASL password for {user}: ")).map_err(unreadable)
}

fn unreadable(err: impl std::fmt::Display) -> Error {
    Error::new(Code::Usage, format!("could not read the password: {err}"))
}

#[cfg(test)]
mod tests {
    use std::fs::{self, Permissions};

    use super::*;

    fn password_file(mode: u32) -> tempfile::NamedTempFile {
        let file = tempfile::NamedTempFile::new().unwrap();
        fs::write(file.path(), "correct horse battery staple\n").unwrap();
        fs::set_permissions(file.path(), Permissions::from_mode(mode)).unwrap();
        file
    }

    #[test]
    fn reads_an_owner_only_file_without_its_newline() {
        let file = password_file(0o600);
        let password = read(&Source::File(file.path().into())).unwrap();
        assert_eq!(password.expose(), Some("correct horse battery staple"));
    }

    #[test]
    fn refuses_a_file_others_can_read() {
        let file = password_file(0o644);
        let err = read(&Source::File(file.path().into())).unwrap_err();
        assert_eq!(err.code, Code::Usage);
        assert_eq!(err.hint.as_deref(), Some("chmod 600 it"));
        assert!(!err.to_string().contains("horse"));
    }

    #[test]
    fn reads_a_systemd_credential_whatever_its_group_bits() {
        let credential = password_file(0o440);
        let dir = credential.path().parent();
        let password = from_file(credential.path(), dir).unwrap();
        assert_eq!(password, "correct horse battery staple\n");

        let elsewhere = Path::new("/run/credentials/other.service");
        for credentials_dir in [None, Some(elsewhere)] {
            let err = from_file(credential.path(), credentials_dir).unwrap_err();
            assert_eq!(err.hint.as_deref(), Some("chmod 600 it"));
        }
    }

    #[test]
    fn refuses_a_world_readable_systemd_credential() {
        let credential = password_file(0o444);
        let err = from_file(credential.path(), credential.path().parent()).unwrap_err();
        assert_eq!(err.hint.as_deref(), Some("chmod 600 it"));
    }
}
