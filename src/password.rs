//! The bot's SASL password, read once at `connect` and never from argv or env.

use std::fs::File;
use std::io::{self, IsTerminal, Read};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use crate::error::{Code, Error, Result};
use crate::irc::Secret;

/// `connect` writes the password and closes the pipe right after spawning the daemon.
const STDIN_TIMEOUT: Duration = Duration::from_secs(10);

pub enum Source {
    /// A file only its owner can read, e.g. a systemd credential.
    File(PathBuf),
    /// A pipe from the `connect` that spawned this daemon.
    Stdin,
    /// A no-echo prompt for this SASL account.
    Tty(String),
}

pub fn read(source: &Source) -> Result<Secret> {
    let mut password = match source {
        Source::File(path) => from_file(path)?,
        Source::Stdin => read_within(io::stdin(), STDIN_TIMEOUT)?,
        Source::Tty(user) => from_tty(user)?,
    };
    password.truncate(password.trim_end_matches(['\r', '\n']).len());
    Ok(Secret::new(password))
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

/// Reads `reader` to its end, unless that takes longer than `timeout`; the reading thread is then
/// left blocked, and dies with the process.
fn read_within(reader: impl Read + Send + 'static, timeout: Duration) -> Result<String> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || sender.send(io::read_to_string(reader)));
    receiver
        .recv_timeout(timeout)
        .map_err(|_| {
            Error::new(
                Code::Usage,
                format!("no password arrived on stdin within {}s", timeout.as_secs()),
            )
            .with_hint("--password-stdin is for `connect` to pass the password to its daemon")
        })?
        .map_err(unreadable)
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
    use std::io::Write;

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
        assert_eq!(password.expose(), "correct horse battery staple");
    }

    #[test]
    fn stdin_gives_up_on_a_pipe_left_open() {
        let (reader, mut writer) = io::pipe().unwrap();
        writer.write_all(b"correct horse").unwrap();
        let error = read_within(reader, Duration::from_millis(50)).unwrap_err();
        assert_eq!(error.code, Code::Usage);
        assert!(!error.to_string().contains("horse"));

        let (reader, mut writer) = io::pipe().unwrap();
        writer.write_all(b"correct horse battery staple\n").unwrap();
        drop(writer);
        let password = read_within(reader, Duration::from_secs(5)).unwrap();
        assert_eq!(password, "correct horse battery staple\n");
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
