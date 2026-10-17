use std::env;
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
            .map_err(|err| no_session(&path, &err))?;
        Ok(Self {
            socket: BufReader::new(stream),
        })
    }

    /// Sends `op` and returns the daemon's result or error.
    pub async fn request(&mut self, op: Op) -> Result<Value> {
        protocol::write_line(&mut self.socket, &Request::new(op))
            .await
            .map_err(|err| broken_connection(&err))?;
        protocol::read_line(&mut self.socket)
            .await
            .map_err(|err| broken_connection(&err))?
            .ok_or_else(|| {
                broken_connection(&std::io::Error::other(
                    "the daemon closed the connection without answering",
                ))
            })?
    }

    /// Tells the daemon the CLI has printed the last `wait` result, so it
    /// can advance the consumer cursor. Call only after printing.
    pub async fn ack(&mut self) -> Result<()> {
        protocol::write_line(&mut self.socket, &Ack { ack: true })
            .await
            .map_err(|err| broken_connection(&err))
    }
}

fn no_session(path: &Path, err: &std::io::Error) -> Error {
    Error::new(
        Code::NoSession,
        format!("could not reach louro at {} ({err})", path.display()),
    )
    .with_hint("louro isn't running; ask your operator to start it")
}

fn broken_connection(err: &std::io::Error) -> Error {
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

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::UnixListener;

    use super::*;
    use crate::protocol::{Kind, SendArgs};

    #[tokio::test]
    async fn connecting_to_a_missing_socket_is_no_session() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("louro.sock");

        let err = Connection::connect(Some(&socket)).await.unwrap_err();

        assert_eq!(err.code, Code::NoSession);
        assert_eq!(
            err.hint.as_deref(),
            Some("louro isn't running; ask your operator to start it")
        );
    }

    #[tokio::test]
    async fn wait_prints_before_it_acks() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("louro.sock");
        let listener = UnixListener::bind(&socket).unwrap();

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = BufReader::new(stream);
            let request: Request = protocol::read_line(&mut stream).await.unwrap().unwrap();
            assert_eq!(
                request.op,
                Op::Send(SendArgs {
                    target: "#bots".into(),
                    text: "maria just joined".into(),
                    kind: Kind::Notice,
                })
            );
            let response: Result<Value> = Ok(serde_json::json!({
                "target": "#bots", "msgid": "9qq1v0x3hdk2a", "lines": 1
            }));
            protocol::write_line(&mut stream, &response).await.unwrap();

            let ack: Ack = protocol::read_line(&mut stream).await.unwrap().unwrap();
            ack
        });

        let mut connection = Connection::connect(Some(&socket)).await.unwrap();
        let result = connection
            .request(Op::Send(SendArgs {
                target: "#bots".into(),
                text: "maria just joined".into(),
                kind: Kind::Notice,
            }))
            .await
            .unwrap();
        assert_eq!(result["msgid"], "9qq1v0x3hdk2a");

        connection.ack().await.unwrap();
        let ack = server.await.unwrap();
        assert!(ack.ack);
    }

    #[tokio::test]
    async fn an_eof_before_a_response_is_an_internal_error() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("louro.sock");
        let listener = UnixListener::bind(&socket).unwrap();

        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 1];
            let _ = stream.read(&mut buf).await;
            stream.shutdown().await.unwrap();
        });

        let mut connection = Connection::connect(Some(&socket)).await.unwrap();
        let err = connection.request(Op::Status {}).await.unwrap_err();
        assert_eq!(err.code, Code::Internal);
        server.await.unwrap();
    }
}
