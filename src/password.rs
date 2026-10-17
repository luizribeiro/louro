//! The bot's SASL password, read once at `connect` and never from argv or env.

use std::fs::File;
use std::io::{self, IsTerminal};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use crate::error::{Code, Error, Result};

pub enum Source {
    /// A file only its owner can read, e.g. a systemd credential.
    File(PathBuf),
    /// A pipe from the `connect` that spawned this daemon.
    Stdin,
    /// A no-echo prompt for this SASL account.
    Tty(String),
}

pub fn read(source: &Source) -> Result<String> {
    let mut password = match source {
        Source::File(path) => from_file(path)?,
        Source::Stdin => io::read_to_string(io::stdin()).map_err(unreadable)?,
        Source::Tty(user) => from_tty(user)?,
    };
    password.truncate(password.trim_end_matches(['\r', '\n']).len());
    Ok(password)
}

fn from_file(path: &Path) -> Result<String> {
    let file = File::open(path).map_err(unreadable)?;
    if file.metadata().map_err(unreadable)?.permissions().mode() & 0o077 != 0 {
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
        .with_hint("run `louro connect` in your own terminal, or use --password-file"));
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

    fn password_file(contents: &str, mode: u32) -> tempfile::NamedTempFile {
        let file = tempfile::NamedTempFile::new().unwrap();
        fs::write(file.path(), contents).unwrap();
        fs::set_permissions(file.path(), Permissions::from_mode(mode)).unwrap();
        file
    }

    #[test]
    fn reads_an_owner_only_file_without_its_newline() {
        let file = password_file("correct horse battery staple\n", 0o600);
        let password = read(&Source::File(file.path().into())).unwrap();
        assert_eq!(password, "correct horse battery staple");
    }

    #[test]
    fn refuses_a_file_others_can_read() {
        let file = password_file("correct horse battery staple\n", 0o644);
        let err = read(&Source::File(file.path().into())).unwrap_err();
        assert_eq!(err.code, Code::Usage);
        assert_eq!(err.hint.as_deref(), Some("chmod 600 it"));
        assert!(!err.to_string().contains("horse"));
    }
}
