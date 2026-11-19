use std::io::{self, Write};
use std::num::NonZeroUsize;
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use clap::Subcommand;
use serde_json::Value;
use tokio::time::{self, Instant};

use crate::error::{Code, Error, Result};
use crate::irc::whole_seconds;
use crate::output;
use crate::socket::{self, Connection, DEFAULT_MAX, Only, Op};

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
        Command::Request(Op::Wait {
            only, follow: true, ..
        }) => follow(None, &only, &mut io::stdout()).await,
        Command::Request(Op::Wait {
            timeout, max, only, ..
        }) => wait(timeout, max, only).await,
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
async fn wait(timeout: u64, max: NonZeroUsize, only: Vec<Only>) -> ExitCode {
    match wait_through_restarts(None, timeout, max, &only).await {
        Ok((mut connection, batch)) => {
            let exit_code = output::emit(Ok(batch));
            let _ = connection.ack().await;
            exit_code
        }
        Err(err) => output::emit::<()>(Err(err)),
    }
}

/// What one try at a wait came to.
enum Tried {
    Answered(Result<(Connection, Value)>),
    /// No daemon is listening, as while louro restarts.
    Absent(Error),
    /// A daemon took the request and hung up without answering.
    Stopped(Error),
}

async fn try_wait(socket: Option<&Path>, timeout: u64, max: NonZeroUsize, only: &[Only]) -> Tried {
    match Connection::connect(socket).await {
        Ok(mut connection) => {
            let wait = Op::Wait {
                timeout,
                max,
                only: only.to_vec(),
                follow: false,
            };
            match connection.answer(wait).await {
                Ok(reply) => Tried::Answered(reply.map(|batch| (connection, batch))),
                Err(stopped) => Tried::Stopped(socket::louro_stopped(stopped)),
            }
        }
        Err(err) if err.code == Code::NoSession => Tried::Absent(err),
        Err(err) => Tried::Answered(Err(err)),
    }
}

/// Sends the wait again, with the time left, to whichever daemon answers at `socket` next.
async fn wait_through_restarts(
    socket: Option<&Path>,
    timeout: u64,
    max: NonZeroUsize,
    only: &[Only],
) -> Result<(Connection, Value)> {
    let deadline = Instant::now().checked_add(Duration::from_secs(timeout));
    let time_left = || deadline.map(|deadline| deadline.saturating_duration_since(Instant::now()));
    let mut gone_since = Instant::now();
    let mut pause = FIRST_RETRY;
    loop {
        let timeout = time_left().map_or(u64::MAX, whole_seconds);
        let gone = match try_wait(socket, timeout, max, only).await {
            Tried::Answered(answer) => return answer,
            Tried::Absent(gone) => gone,
            Tried::Stopped(gone) => {
                gone_since = Instant::now();
                pause = FIRST_RETRY;
                gone
            }
        };
        let left = time_left().unwrap_or(Duration::MAX);
        if left.is_zero() || gone_since.elapsed() >= GIVE_UP {
            return Err(gone);
        }
        time::sleep(pause.min(left)).await;
        pause = (pause * 2).min(MAX_RETRY);
    }
}

/// Prints each event as its own line, acking a batch only after it is printed. Ends when stdout
/// closes, which leaves the unacked events with the daemon, or on an error a retry can't fix.
async fn follow(socket: Option<&Path>, only: &[Only], out: &mut impl Write) -> ExitCode {
    follow_events(socket, only, out)
        .await
        .unwrap_or(ExitCode::SUCCESS)
}

async fn follow_events(
    socket: Option<&Path>,
    only: &[Only],
    out: &mut impl Write,
) -> io::Result<ExitCode> {
    let mut pause = FIRST_RETRY;
    let mut reported = false;
    loop {
        let gone = match try_wait(socket, u64::MAX, DEFAULT_MAX, only).await {
            Tried::Answered(Ok((mut connection, batch))) => {
                for event in batch["events"].as_array().into_iter().flatten() {
                    writeln!(out, "{event}")?;
                }
                out.flush()?;
                let _ = connection.ack().await;
                (pause, reported) = (FIRST_RETRY, false);
                continue;
            }
            Tried::Answered(Err(error)) => {
                output::report(out, &error)?;
                return Ok(ExitCode::from(error.code.exit_code()));
            }
            Tried::Absent(gone) => gone,
            Tried::Stopped(gone) => {
                (pause, reported) = (FIRST_RETRY, false);
                gone
            }
        };
        if !reported {
            output::report(out, &gone)?;
            out.flush()?;
            reported = true;
        }
        time::sleep(pause).await;
        pause = (pause * 2).min(MAX_RETRY);
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::{Arc, Mutex};

    use serde_json::json;
    use tempfile::tempdir;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::{UnixListener, UnixStream};

    use super::*;

    const TWENTY: NonZeroUsize = NonZeroUsize::new(20).unwrap();

    /// Accepts one connection on `listener`, returning it and its request.
    async fn accept_request(listener: &UnixListener) -> (BufReader<UnixStream>, Value) {
        let (stream, _) = listener.accept().await.unwrap();
        let mut stream = BufReader::new(stream);
        let mut request = String::new();
        stream.read_line(&mut request).await.unwrap();
        (stream, serde_json::from_str(&request).unwrap())
    }

    /// Like [`accept_request`], returning only its wait request's timeout.
    async fn accept_wait(listener: &UnixListener) -> (BufReader<UnixStream>, Value) {
        let (stream, request) = accept_request(listener).await;
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

            let (mut connection, batch) = wait_through_restarts(Some(&path), timeout, TWENTY, &[])
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
        let gone =
            wait_through_restarts(Some(&dir.path().join("louro.sock")), u64::MAX, TWENTY, &[])
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
        let gone = wait_through_restarts(Some(&dir.path().join("louro.sock")), 4, TWENTY, &[])
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
            let refused = wait_through_restarts(Some(&path), u64::MAX, TWENTY, &[])
                .await
                .err()
                .unwrap();
            assert!(refused.message.contains(refusal), "{refused}");
            daemon.await.unwrap();
        }
    }

    /// A stdout that daemons write "ack" into too, to show what came first.
    #[derive(Clone, Default)]
    struct Transcript(Arc<Mutex<String>>);

    impl Transcript {
        fn note(&self, line: &str) {
            let mut text = self.0.lock().unwrap();
            text.push_str(line);
            text.push('\n');
        }

        fn lines(&self) -> Vec<String> {
            self.0.lock().unwrap().lines().map(String::from).collect()
        }
    }

    impl Write for Transcript {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0
                .lock()
                .unwrap()
                .push_str(&String::from_utf8_lossy(buf));
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn message(id: u64, text: &str) -> Value {
        json!({"id": id, "type": "message", "text": text, "untrusted": true})
    }

    /// Serves one batch of `events` to the next wait on `listener`, and notes the ack.
    async fn serve_batch(
        listener: &UnixListener,
        events: &[Value],
        transcript: &Transcript,
    ) -> Value {
        let (mut stream, request) = accept_request(listener).await;
        let batch =
            json!({"Ok": {"events": events, "timed_out": false, "notice": "n", "now": "t"}});
        reply(&mut stream, batch).await;
        let mut ack = String::new();
        stream.read_line(&mut ack).await.unwrap();
        assert_eq!(ack.trim(), r#"{"ack":true}"#);
        transcript.note("ack");
        request
    }

    #[tokio::test(start_paused = true)]
    async fn follow_prints_each_event_as_a_line_and_acks_each_batch_after_printing() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("louro.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let transcript = Transcript::default();
        let first = [message(1, "louro: ping"), message(2, "hey, got a sec?")];
        let second = [message(3, "it's about ergo")];

        let daemon = async {
            let request = serve_batch(&listener, &first, &transcript).await;
            assert_eq!(request["args"]["timeout"], json!(u64::MAX));
            assert_eq!(request["args"]["only"], json!(["dms", "mentions"]));
            serve_batch(&listener, &second, &transcript).await;
        };
        let only = [Only::Dms, Only::Mentions];
        let mut out = transcript.clone();
        tokio::select! {
            _ = follow(Some(&path), &only, &mut out) => panic!("follow ended"),
            () = daemon => {}
        }

        let line = |event: &Value| event.to_string();
        assert_eq!(
            transcript.lines(),
            [
                line(&first[0]),
                line(&first[1]),
                "ack".into(),
                line(&second[0]),
                "ack".into()
            ]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn follow_reports_an_outage_once_and_carries_on_with_the_next_daemon() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("louro.sock");
        let transcript = Transcript::default();
        let events = [message(1, "louro: ping")];

        let daemon = async {
            time::sleep(Duration::from_secs(300)).await;
            let listener = UnixListener::bind(&path).unwrap();
            serve_batch(&listener, &events, &transcript).await;
        };
        let mut out = transcript.clone();
        tokio::select! {
            _ = follow(Some(&path), &[], &mut out) => panic!("follow ended"),
            () = daemon => {}
        }

        let lines = transcript.lines();
        assert_eq!(lines.len(), 3, "{lines:?}");
        let outage: Value = serde_json::from_str(&lines[0]).unwrap();
        assert_eq!(outage["error"]["code"], "no_session");
        assert_eq!(lines[1..], [events[0].to_string(), "ack".into()]);
    }

    #[tokio::test(start_paused = true)]
    async fn follow_ends_with_the_error_code_when_a_retry_cannot_help() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("louro.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let daemon = tokio::spawn(async move {
            let (mut stream, _) = accept_request(&listener).await;
            let mismatch =
                json!({"Err": {"code": "version_mismatch", "message": "different versions"}});
            reply(&mut stream, mismatch).await;
        });

        let mut out = Transcript::default();
        let code = follow(Some(&path), &[], &mut out).await;
        daemon.await.unwrap();

        assert_eq!(format!("{code:?}"), format!("{:?}", ExitCode::from(11)));
        let lines = out.lines();
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(
            lines[0].contains(r#""code":"version_mismatch""#),
            "{lines:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn follow_reports_again_when_a_daemon_dies_after_taking_the_wait() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("louro.sock");
        let transcript = Transcript::default();
        let events = [message(1, "louro: ping")];

        let daemons = async {
            time::sleep(Duration::from_secs(10)).await;
            let first = UnixListener::bind(&path).unwrap();
            drop(accept_request(&first).await);
            drop(first);
            fs::remove_file(&path).unwrap();
            time::sleep(Duration::from_secs(30)).await;
            let second = UnixListener::bind(&path).unwrap();
            serve_batch(&second, &events, &transcript).await;
        };
        let mut out = transcript.clone();
        tokio::select! {
            _ = follow(Some(&path), &[], &mut out) => panic!("follow ended"),
            () = daemons => {}
        }

        let lines = transcript.lines();
        assert_eq!(lines.len(), 4, "{lines:?}");
        for outage in &lines[..2] {
            let outage: Value = serde_json::from_str(outage).unwrap();
            assert_eq!(outage["error"]["code"], "no_session");
        }
        assert_eq!(lines[2..], [events[0].to_string(), "ack".into()]);
    }
}
