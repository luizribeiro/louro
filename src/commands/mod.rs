use std::num::NonZeroUsize;
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use clap::Subcommand;
use serde_json::Value;
use tokio::time::{self, Instant};

use crate::error::{Code, Result};
use crate::irc::whole_seconds;
use crate::output;
use crate::socket::{self, Connection, Op};

pub mod connect;
mod send;

#[derive(Subcommand)]
pub enum Command {
    /// Send a message, notice or action
    Send(send::Args),
    /// Start the daemon (operator only)
    Connect(connect::Args),
    #[command(flatten)]
    Request(Op),
}

pub async fn dispatch(command: Command) -> ExitCode {
    match command {
        Command::Send(args) => args.run().await,
        Command::Connect(args) => args.run().await,
        Command::Request(Op::Wait { timeout, max }) => wait(timeout, max).await,
        Command::Request(op) => output::emit(socket::request(op).await),
    }
}

/// How soon `wait` looks again for a daemon that went away, as while louro restarts.
const FIRST_RETRY: Duration = Duration::from_secs(1);
/// The longest it backs off to between looks.
const MAX_RETRY: Duration = Duration::from_secs(5);
/// Without any daemon for this long, louro was stopped rather than restarted.
const GIVE_UP: Duration = Duration::from_secs(120);

/// Acks only after printing, so the daemon never drops events the agent didn't see.
async fn wait(timeout: u64, max: NonZeroUsize) -> ExitCode {
    match wait_through_restarts(None, timeout, max).await {
        Ok((mut connection, batch)) => {
            let exit_code = output::emit(Ok(batch));
            let _ = connection.ack().await;
            exit_code
        }
        Err(err) => output::emit::<()>(Err(err)),
    }
}

/// Sends the wait again, with the time left, to whichever daemon answers at `socket` next.
async fn wait_through_restarts(
    socket: Option<&Path>,
    timeout: u64,
    max: NonZeroUsize,
) -> Result<(Connection, Value)> {
    let deadline = Instant::now().checked_add(Duration::from_secs(timeout));
    let time_left = || deadline.map(|deadline| deadline.saturating_duration_since(Instant::now()));
    let mut gone_since = Instant::now();
    let mut pause = FIRST_RETRY;
    loop {
        let timeout = time_left().map_or(u64::MAX, whole_seconds);
        let gone = match Connection::connect(socket).await {
            Ok(mut connection) => match connection.answer(Op::Wait { timeout, max }).await {
                Ok(reply) => return reply.map(|batch| (connection, batch)),
                Err(stopped) => {
                    gone_since = Instant::now();
                    pause = FIRST_RETRY;
                    socket::louro_stopped(stopped)
                }
            },
            Err(err) if err.code == Code::NoSession => err,
            Err(err) => return Err(err),
        };
        let left = time_left().unwrap_or(Duration::MAX);
        if left.is_zero() || gone_since.elapsed() >= GIVE_UP {
            return Err(gone);
        }
        time::sleep(pause.min(left)).await;
        pause = (pause * 2).min(MAX_RETRY);
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tempfile::tempdir;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::{UnixListener, UnixStream};

    use super::*;

    const TWENTY: NonZeroUsize = NonZeroUsize::new(20).unwrap();

    /// Accepts one connection on `listener`, returning it and its wait request's timeout.
    async fn accept_wait(listener: &UnixListener) -> (BufReader<UnixStream>, Value) {
        let (stream, _) = listener.accept().await.unwrap();
        let mut stream = BufReader::new(stream);
        let mut request = String::new();
        stream.read_line(&mut request).await.unwrap();
        let request: Value = serde_json::from_str(&request).unwrap();
        (stream, request["args"]["timeout"].clone())
    }

    async fn reply(stream: &mut BufReader<UnixStream>, line: Value) {
        stream
            .write_all(format!("{line}\n").as_bytes())
            .await
            .unwrap();
    }

    /// The old daemon holds the wait for 100.5s, then louro is down for 30s, past `GIVE_UP` since
    /// the wait began. Retries come 1s, 2s, 4s, then 5s apart, so the new daemon gets the wait at
    /// 132.5s, with 167.5s of 300s left.
    #[tokio::test(start_paused = true)]
    async fn a_wait_rides_out_a_restart_with_the_time_it_has_left() {
        for (timeout, resent) in [(300, json!(168)), (u64::MAX, json!(u64::MAX))] {
            let dir = tempdir().unwrap();
            let path = dir.path().join("louro.sock");
            let old = UnixListener::bind(&path).unwrap();
            let restart = path.clone();
            let daemons = tokio::spawn(async move {
                let (stopping, first) = accept_wait(&old).await;
                time::sleep(Duration::from_millis(100_500)).await;
                drop((stopping, old));
                time::sleep(Duration::from_secs(30)).await;
                std::fs::remove_file(&restart).unwrap();
                let new = UnixListener::bind(&restart).unwrap();
                let (mut serving, second) = accept_wait(&new).await;
                reply(
                    &mut serving,
                    json!({"Ok": {"events": [], "timed_out": false}}),
                )
                .await;
                let mut ack = String::new();
                serving.read_line(&mut ack).await.unwrap();
                (first, second, ack)
            });

            let (mut connection, batch) = wait_through_restarts(Some(&path), timeout, TWENTY)
                .await
                .unwrap();
            assert_eq!(batch["timed_out"], false);
            connection.ack().await.unwrap();
            let (first, second, ack) = daemons.await.unwrap();
            assert_eq!((first, second), (json!(timeout), resent));
            assert_eq!(ack.trim(), r#"{"ack":true}"#);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_wait_gives_up_after_two_minutes_without_a_daemon() {
        let dir = tempdir().unwrap();
        let started = Instant::now();
        let gone = wait_through_restarts(Some(&dir.path().join("louro.sock")), u64::MAX, TWENTY)
            .await
            .err()
            .unwrap();
        assert_eq!(gone.code, Code::NoSession);
        assert!((GIVE_UP..GIVE_UP + MAX_RETRY).contains(&started.elapsed()));
    }

    #[tokio::test(start_paused = true)]
    async fn a_wait_without_a_daemon_ends_at_its_own_timeout() {
        let dir = tempdir().unwrap();
        let started = Instant::now();
        let gone = wait_through_restarts(Some(&dir.path().join("louro.sock")), 4, TWENTY)
            .await
            .err()
            .unwrap();
        assert_eq!(gone.code, Code::NoSession);
        assert_eq!(started.elapsed(), Duration::from_secs(4));
    }

    /// The daemon hangs up after its one reply, so a retry would end in "could not reach louro".
    #[tokio::test(start_paused = true)]
    async fn a_wait_does_not_retry_a_reply_it_cannot_use() {
        let version_mismatch =
            json!({"Err": {"code": "version_mismatch", "message": "different versions"}});
        for (line, refusal) in [
            (version_mismatch, "speaks a different protocol version"),
            (json!("SSH-2.0-OpenSSH_9.6"), "not a louro response"),
        ] {
            let dir = tempdir().unwrap();
            let path = dir.path().join("louro.sock");
            let listener = UnixListener::bind(&path).unwrap();
            let daemon = tokio::spawn(async move {
                let (mut stream, _) = accept_wait(&listener).await;
                reply(&mut stream, line).await;
            });
            let refused = wait_through_restarts(Some(&path), u64::MAX, TWENTY)
                .await
                .err()
                .unwrap();
            assert!(refused.message.contains(refusal), "{refused}");
            daemon.await.unwrap();
        }
    }
}
