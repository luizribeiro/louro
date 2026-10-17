use std::env;
use std::ffi::OsString;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use serde_json::Value;
use tokio::io::BufReader;
use tokio::net::UnixStream;

use crate::error::{Code, Error, Result};
use crate::protocol::{self, Ack, Op, Request};

/// One request-response round trip with the daemon over its Unix socket.
///
/// Kept open after `request` so `wait` can send its `Ack` on the same
/// connection, once the CLI has printed the result (§4.4: the consumer
/// cursor only advances after that ack).
#[derive(Debug)]
pub struct Connection {
    socket: BufReader<UnixStream>,
}

impl Connection {
    /// # Errors
    ///
    /// `no_session` if the socket is missing or refuses the connection.
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
    ///
    /// # Errors
    ///
    /// The daemon's own error, `version_mismatch`, or `internal` if the
    /// connection breaks after it was established.
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
    ///
    /// # Errors
    ///
    /// `internal` if the connection breaks before the ack is sent.
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

/// `--socket`, else `$XDG_RUNTIME_DIR/louro/louro.sock`, else
/// `$TMPDIR/louro-<uid>/louro.sock` (design §4.2).
///
/// # Errors
///
/// `internal` if the user id for the fallback path can't be determined.
pub fn resolve_socket_path(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return Ok(path.to_path_buf());
    }
    socket_path_from_env(env::var_os("XDG_RUNTIME_DIR"), env::var_os("TMPDIR"))
}

/// The env-driven half of [`resolve_socket_path`], taken as plain values so
/// tests can exercise it without mutating the process environment.
fn socket_path_from_env(
    xdg_runtime_dir: Option<OsString>,
    fallback_tmpdir: Option<OsString>,
) -> Result<PathBuf> {
    if let Some(dir) = xdg_runtime_dir.filter(|dir| !dir.is_empty()) {
        return Ok(PathBuf::from(dir).join("louro").join("louro.sock"));
    }
    let base_dir = fallback_tmpdir
        .filter(|dir| !dir.is_empty())
        .map_or_else(|| PathBuf::from("/tmp"), PathBuf::from);
    let uid = current_uid(&base_dir)?;
    Ok(base_dir.join(format!("louro-{uid}")).join("louro.sock"))
}

/// The current user's id, read from a file this process just created in
/// `base_dir` (so it is a path we own, regardless of who owns `base_dir`
/// itself) via `std::os::unix::fs::MetadataExt`, with no unsafe code.
/// `create_new` makes sure we stat a file we created, not one already
/// sitting at this guessable name.
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

    #[test]
    fn explicit_socket_wins_over_env() {
        let path = resolve_socket_path(Some(Path::new("/tmp/explicit.sock"))).unwrap();
        assert_eq!(path, PathBuf::from("/tmp/explicit.sock"));
    }

    #[test]
    fn falls_back_to_xdg_runtime_dir() {
        let path = socket_path_from_env(Some("/run/user/501".into()), Some("/tmp".into())).unwrap();
        assert_eq!(path, PathBuf::from("/run/user/501/louro/louro.sock"));
    }

    #[test]
    fn falls_back_to_tmpdir_with_uid_when_no_runtime_dir() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().to_str().unwrap();
        let path = socket_path_from_env(None, Some(base.into())).unwrap();
        let uid = current_uid(dir.path()).unwrap();
        assert_eq!(
            path,
            PathBuf::from(format!("{base}/louro-{uid}/louro.sock"))
        );
    }

    #[test]
    fn empty_env_vars_are_treated_as_unset() {
        let path = socket_path_from_env(Some(String::new().into()), None).unwrap();
        let uid = current_uid(Path::new("/tmp")).unwrap();
        assert_eq!(path, PathBuf::from(format!("/tmp/louro-{uid}/louro.sock")));
    }

    #[test]
    fn current_uid_matches_a_freshly_created_file_in_the_same_dir() {
        let dir = tempfile::tempdir().unwrap();
        let probe = tempfile::NamedTempFile::new_in(dir.path()).unwrap();
        let expected = probe.path().metadata().unwrap().uid();
        assert_eq!(current_uid(dir.path()).unwrap(), expected);
    }

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
