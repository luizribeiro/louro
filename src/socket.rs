use std::env;
use std::fmt::Display;
use std::num::NonZeroUsize;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{self, AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use crate::error::{Code, Error, Result};
use crate::irc::{Kind, Typing};

pub const VERSION: u8 = 1;

#[derive(Deserialize, Serialize)]
pub struct Request {
    pub v: u8,
    #[serde(flatten)]
    pub op: Op,
}

impl Request {
    pub const fn new(op: Op) -> Self {
        Self { v: VERSION, op }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "op", content = "args", rename_all = "snake_case")]
pub enum Op {
    Status {},
    Wait {
        timeout: u64,
        max: NonZeroUsize,
    },
    Send {
        target: String,
        text: String,
        kind: Kind,
        reply_to: Option<String>,
    },
    Names {
        channel: String,
    },
    React {
        target: String,
        msgid: String,
        reaction: String,
        remove: bool,
    },
    Typing {
        target: String,
        state: Typing,
    },
    History {
        target: String,
        limit: usize,
        before: Option<String>,
        after: Option<String>,
    },
    Quit {},
}

/// What the client writes after printing a wait result; only then does the daemon move its cursor.
#[derive(Deserialize, Serialize)]
pub struct Ack {
    pub ack: bool,
}

/// Reads one JSON line, or `None` once the peer has closed the connection.
pub async fn read_line<T: DeserializeOwned>(
    reader: &mut (impl AsyncBufRead + Unpin),
) -> io::Result<Option<T>> {
    let mut line = String::new();
    if reader.read_line(&mut line).await? == 0 {
        return Ok(None);
    }
    Ok(Some(serde_json::from_str(&line)?))
}

pub async fn write_line(
    writer: &mut (impl AsyncWrite + Unpin),
    value: &impl Serialize,
) -> io::Result<()> {
    let mut line = serde_json::to_vec(value)?;
    line.push(b'\n');
    writer.write_all(&line).await?;
    writer.flush().await
}

/// Kept open after `request` so `wait` can ack on the same connection.
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
        write_line(&mut self.socket, &Request::new(op))
            .await
            .map_err(louro_stopped)?;
        read_line(&mut self.socket)
            .await
            .map_err(louro_stopped)?
            .ok_or_else(|| louro_stopped("it closed the connection without answering"))?
    }

    /// Tells the daemon the CLI has printed the last `wait` result, so it
    /// can advance the consumer cursor. Call only after printing.
    pub async fn ack(&mut self) -> Result<()> {
        write_line(&mut self.socket, &Ack { ack: true })
            .await
            .map_err(louro_stopped)
    }
}

/// One-shot request/response for commands that don't need to keep the connection open.
pub async fn request(socket: Option<&Path>, op: Op) -> Result<Value> {
    Connection::connect(socket).await?.request(op).await
}

fn no_session(path: &Path, err: impl Display) -> Error {
    Error::new(
        Code::NoSession,
        format!("could not reach louro at {} ({err})", path.display()),
    )
    .with_hint("louro isn't running; ask your operator to start it")
}

/// The daemon was reachable but went away mid-request, e.g. stopped while a `wait` blocked.
fn louro_stopped(err: impl Display) -> Error {
    Error::new(Code::NotConnected, format!("louro stopped ({err})"))
        .with_hint("run `louro status`; if louro isn't running, ask your operator to start it")
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
