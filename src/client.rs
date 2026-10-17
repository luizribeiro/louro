use std::env;
use std::fmt::Display;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use serde_json::Value;
use tokio::io::BufReader;
use tokio::net::UnixStream;

use crate::error::{Code, Error, Result};
use crate::protocol::{self, Ack, Op, Request};

/// Kept open after `request` so `wait` can ack on the same connection.
#[derive(Debug)]
pub struct Connection {
    socket: BufReader<UnixStream>,
}

impl Connection {
    pub async fn connect(socket: Option<&Path>) -> Result<Self> {
        let path = resolve_socket_path(socket)?;
        let stream = UnixStream::connect(&path)
            .await
            .map_err(|err| no_session(&path, err))?;
        Ok(Self {
            socket: BufReader::new(stream),
        })
    }

    /// Sends `op` and returns the daemon's result or error.
    pub async fn request(&mut self, op: Op) -> Result<Value> {
        protocol::write_line(&mut self.socket, &Request::new(op))
            .await
            .map_err(broken_connection)?;
        protocol::read_line(&mut self.socket)
            .await
            .map_err(broken_connection)?
            .ok_or_else(|| {
                broken_connection("the daemon closed the connection without answering")
            })?
    }

    /// Tells the daemon the CLI has printed the last `wait` result, so it
    /// can advance the consumer cursor. Call only after printing.
    pub async fn ack(&mut self) -> Result<()> {
        protocol::write_line(&mut self.socket, &Ack { ack: true })
            .await
            .map_err(broken_connection)
    }
}

fn no_session(path: &Path, err: impl Display) -> Error {
    Error::new(
        Code::NoSession,
        format!("could not reach louro at {} ({err})", path.display()),
    )
    .with_hint("louro isn't running; ask your operator to start it")
}

fn broken_connection(err: impl Display) -> Error {
    Error::new(
        Code::Internal,
        format!("lost the connection to louro: {err}"),
    )
}

/// `--socket`, else `$XDG_RUNTIME_DIR/louro/louro.sock`, else `$TMPDIR/louro-<uid>/louro.sock`.
pub fn resolve_socket_path(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return Ok(path.to_path_buf());
    }
    if let Some(dir) = env::var_os("XDG_RUNTIME_DIR").filter(|dir| !dir.is_empty()) {
        return Ok(PathBuf::from(dir).join("louro/louro.sock"));
    }
    let tmp = env::temp_dir();
    Ok(tmp.join(format!("louro-{}/louro.sock", current_uid(&tmp)?)))
}

/// Safe Rust has no getuid, so this reads the owner of a file it creates; `create_new` makes sure
/// that file isn't one someone else left at this guessable name.
fn current_uid(base_dir: &Path) -> Result<u32> {
    let probe = base_dir.join(format!("louro-uid-probe-{}", std::process::id()));
    let uid = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
        .and_then(|_| std::fs::metadata(&probe))
        .map(|metadata| metadata.uid());
    let _ = std::fs::remove_file(&probe);
    uid.map_err(|err| {
        Error::new(
            Code::Internal,
            format!("could not determine the user id for the socket path: {err}"),
        )
    })
}
