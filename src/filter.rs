//! The operator's outgoing filter: a program that reads each text louro would show others on
//! IRC and passes, rewrites or blocks it. One process per text, given the text on stdin and the
//! context in `LOURO_*` variables.

use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use rustix::process::{Pid, Signal, kill_process_group};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};
use tokio::sync::Semaphore;

const PROTOCOL_VERSION: &str = "1";
/// The exit code that blocks a text; 11 to 19 are reserved for finer blocks.
const BLOCKED: i32 = 10;
const MAX_OUTPUT_BYTES: usize = 64 * 1024;
const MAX_RUNNING: usize = 4;

pub struct Filter {
    argv: Vec<String>,
    timeout: Duration,
    server: String,
    running: Semaphore,
}

/// What the filter is told about a text besides the text itself; `None` leaves its variable
/// unset.
#[derive(Default)]
pub struct Context {
    pub kind: &'static str,
    pub channel: Option<String>,
    pub nick: Option<String>,
    pub account: Option<String>,
    pub msgid: Option<String>,
    pub self_nick: String,
    pub self_account: Option<String>,
}

#[derive(Debug, PartialEq)]
pub enum Verdict {
    Pass,
    Rewritten(String),
    Blocked,
    /// Why the filter gave no verdict; nothing may be sent.
    Failed(String),
}

pub struct Outcome {
    pub verdict: Verdict,
    /// The filter's note to the agent, raw.
    pub stderr: String,
}

impl Filter {
    pub fn new(argv: Vec<String>, timeout: Duration, server: String) -> Self {
        Self {
            argv,
            timeout,
            server,
            running: Semaphore::new(MAX_RUNNING),
        }
    }

    pub async fn run(&self, text: &str, context: &Context) -> Outcome {
        let _running = self.running.acquire().await.expect("never closed");
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        let verdict = match self.command(context).spawn() {
            Ok(mut child) => {
                let talked = tokio::time::timeout(
                    self.timeout,
                    talk(&mut child, text, &mut stdout, &mut stderr),
                )
                .await
                .unwrap_or_else(|_| Err(format!("it ran past {:?}", self.timeout)));
                match talked {
                    Ok(status) => verdict(text, status, stdout),
                    Err(why) => {
                        kill_group(&child);
                        Verdict::Failed(why)
                    }
                }
            }
            Err(err) => Verdict::Failed(format!("can't run it: {err}")),
        };
        Outcome {
            verdict,
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
        }
    }

    fn command(&self, context: &Context) -> Command {
        let mut command = Command::new(&self.argv[0]);
        command
            .args(&self.argv[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .kill_on_drop(true);
        for (name, value) in self.env(context) {
            match value {
                Some(value) => command.env(name, value),
                None => command.env_remove(name),
            };
        }
        command
    }

    fn env<'a>(&'a self, context: &'a Context) -> [(&'static str, Option<&'a str>); 9] {
        [
            ("LOURO_FILTER_VERSION", Some(PROTOCOL_VERSION)),
            ("LOURO_KIND", Some(context.kind)),
            ("LOURO_CHANNEL", context.channel.as_deref()),
            ("LOURO_NICK", context.nick.as_deref()),
            ("LOURO_ACCOUNT", context.account.as_deref()),
            ("LOURO_MSGID", context.msgid.as_deref()),
            ("LOURO_SELF_NICK", Some(&context.self_nick)),
            ("LOURO_SELF_ACCOUNT", context.self_account.as_deref()),
            ("LOURO_SERVER", Some(&self.server)),
        ]
    }
}

/// Writes `text` to the filter while reading its output, so neither side blocks on a full pipe.
async fn talk(
    child: &mut Child,
    text: &str,
    stdout: &mut Vec<u8>,
    stderr: &mut Vec<u8>,
) -> Result<ExitStatus, String> {
    let mut stdin = child.stdin.take().expect("piped");
    let write = async move {
        // A filter may exit without reading it all.
        let _ = stdin.write_all(text.as_bytes()).await;
        Ok(())
    };
    let out = read_stdout(child.stdout.take().expect("piped"), stdout);
    let err = read_stderr(child.stderr.take().expect("piped"), stderr);
    tokio::try_join!(write, out, err)?;
    child.wait().await.map_err(|err| err.to_string())
}

async fn read_stdout(reader: impl AsyncRead + Unpin, buffer: &mut Vec<u8>) -> Result<(), String> {
    reader
        .take(MAX_OUTPUT_BYTES as u64 + 1)
        .read_to_end(buffer)
        .await
        .map_err(|err| format!("can't read its output: {err}"))?;
    if buffer.len() > MAX_OUTPUT_BYTES {
        return Err(format!("its output is over {MAX_OUTPUT_BYTES} bytes"));
    }
    Ok(())
}

/// Keeps the start of a long note and drains the rest, so the filter never blocks writing it.
async fn read_stderr(
    mut reader: impl AsyncRead + Unpin,
    buffer: &mut Vec<u8>,
) -> Result<(), String> {
    let _ = (&mut reader)
        .take(MAX_OUTPUT_BYTES as u64)
        .read_to_end(buffer)
        .await;
    let _ = tokio::io::copy(&mut reader, &mut tokio::io::sink()).await;
    Ok(())
}

/// The filter runs in its own process group, so this ends anything it started too.
fn kill_group(child: &Child) {
    if let Some(pid) = child
        .id()
        .and_then(|id| i32::try_from(id).ok())
        .and_then(Pid::from_raw)
    {
        let _ = kill_process_group(pid, Signal::KILL);
    }
}

fn verdict(text: &str, status: ExitStatus, mut stdout: Vec<u8>) -> Verdict {
    match status.code() {
        Some(0) => {}
        Some(BLOCKED) => return Verdict::Blocked,
        _ => return Verdict::Failed(format!("it ended with {status}")),
    }
    if stdout.last() == Some(&b'\n') {
        stdout.pop();
    }
    let Ok(output) = String::from_utf8(stdout) else {
        return Verdict::Failed("its output is not UTF-8".into());
    };
    if output.is_empty() && !text.is_empty() {
        return Verdict::Failed("its output is empty".into());
    }
    if output == text {
        Verdict::Pass
    } else {
        Verdict::Rewritten(output)
    }
}

/// Filters written as shell scripts, for tests.
#[cfg(test)]
pub mod fixtures {
    use std::fs::{self, Permissions};
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::time::Duration;

    use tempfile::TempDir;

    use super::Filter;

    pub fn script(dir: &TempDir, body: &str) -> Filter {
        script_with_timeout(dir, body, Duration::from_secs(5))
    }

    pub fn script_with_timeout(dir: &TempDir, body: &str, timeout: Duration) -> Filter {
        let path = dir.path().join("filter");
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&path, Permissions::from_mode(0o755)).unwrap();
        filter_at(&path, timeout)
    }

    pub fn filter_at(path: &Path, timeout: Duration) -> Filter {
        let argv = vec![path.to_str().unwrap().into()];
        Filter::new(argv, timeout, "irc.example.org".into())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tempfile::TempDir;
    use tokio::task::JoinSet;

    use super::fixtures::{filter_at, script, script_with_timeout};
    use super::*;

    const TOKEN: &str = "ghp_4fQe9TzXb2LmN8vR1sWkYc7Hd0Jp3Ua6Bn5M";

    fn message() -> Context {
        Context {
            kind: "message",
            channel: Some("#bots".into()),
            self_nick: "louro".into(),
            ..Context::default()
        }
    }

    async fn run(filter: &Filter, text: &str) -> Outcome {
        filter.run(text, &message()).await
    }

    fn failed(why: &str) -> Verdict {
        Verdict::Failed(why.into())
    }

    #[tokio::test]
    async fn cat_passes_the_text_unchanged() {
        let dir = TempDir::new().unwrap();
        let outcome = run(&script(&dir, "cat"), "line one\nline two").await;
        assert_eq!(outcome.verdict, Verdict::Pass);
        assert_eq!(outcome.stderr, "");
    }

    #[tokio::test]
    async fn a_redaction_rewrites_the_text_and_leaves_a_note() {
        let dir = TempDir::new().unwrap();
        let filter = script(
            &dir,
            "sed 's/ghp_[A-Za-z0-9]*/[redacted]/g'\necho 'redacted a GitHub token' >&2",
        );
        let outcome = run(&filter, &format!("the token is {TOKEN}\nuse it")).await;
        assert_eq!(
            outcome.verdict,
            Verdict::Rewritten("the token is [redacted]\nuse it".into())
        );
        assert_eq!(outcome.stderr, "redacted a GitHub token\n");
    }

    #[tokio::test]
    async fn exit_10_blocks_with_a_note() {
        let dir = TempDir::new().unwrap();
        let filter = script(
            &dir,
            "echo 'looks like a GitHub token; describe it instead' >&2\nexit 10",
        );
        let outcome = run(&filter, TOKEN).await;
        assert_eq!(outcome.verdict, Verdict::Blocked);
        assert_eq!(
            outcome.stderr,
            "looks like a GitHub token; describe it instead\n"
        );
    }

    #[tokio::test]
    async fn any_other_exit_or_a_signal_fails_with_its_note_kept() {
        let dir = TempDir::new().unwrap();
        for (body, why) in [
            ("exit 3", "it ended with exit status: 3"),
            ("exit 11", "it ended with exit status: 11"),
            ("kill -9 $$", "it ended with signal: 9 (SIGKILL)"),
        ] {
            let filter = script(&dir, &format!("cat\necho oops >&2\n{body}"));
            let outcome = run(&filter, "hi").await;
            assert_eq!(outcome.verdict, failed(why), "{body}");
            assert_eq!(outcome.stderr, "oops\n", "{body}");
        }
    }

    #[tokio::test]
    async fn a_missing_program_fails() {
        let dir = TempDir::new().unwrap();
        let missing = filter_at(&dir.path().join("missing"), Duration::from_secs(5));
        let Verdict::Failed(why) = run(&missing, "hi").await.verdict else {
            panic!("a missing filter gave a verdict");
        };
        assert!(why.starts_with("can't run it: "), "{why}");
    }

    #[tokio::test]
    async fn a_timeout_fails_and_kills_what_the_filter_started() {
        let dir = TempDir::new().unwrap();
        let survived = dir.path().join("survived");
        let filter = script_with_timeout(
            &dir,
            &format!("(sleep 1; touch '{}') &\nsleep 10", survived.display()),
            Duration::from_millis(100),
        );
        let started = std::time::Instant::now();
        let outcome = run(&filter, "hi").await;
        assert_eq!(outcome.verdict, failed("it ran past 100ms"));
        assert!(started.elapsed() < Duration::from_secs(1));

        tokio::time::sleep(Duration::from_millis(1500)).await;
        assert!(!survived.exists(), "the filter's child outlived it");
    }

    #[tokio::test]
    async fn output_is_at_most_64_kib_of_utf8_and_trims_one_newline() {
        let dir = TempDir::new().unwrap();
        let xs = |bytes| format!("cat >/dev/null\nhead -c {bytes} /dev/zero | tr '\\0' x");
        for (body, verdict) in [
            (xs(65_536), Verdict::Rewritten("x".repeat(65_536))),
            (xs(65_537), failed("its output is over 65536 bytes")),
            (xs(100_000), failed("its output is over 65536 bytes")),
            (
                "cat >/dev/null\nprintf '\\377\\376'".into(),
                failed("its output is not UTF-8"),
            ),
            (
                "cat >/dev/null\nprintf 'a\\n\\n'".into(),
                Verdict::Rewritten("a\n".into()),
            ),
        ] {
            assert_eq!(
                run(&script(&dir, &body), "hi").await.verdict,
                verdict,
                "{body}"
            );
        }
    }

    #[tokio::test]
    async fn empty_output_fails_unless_the_input_was_empty() {
        let dir = TempDir::new().unwrap();
        let filter = script(&dir, "cat >/dev/null");
        assert_eq!(
            run(&filter, "hi").await.verdict,
            failed("its output is empty")
        );
        assert_eq!(run(&filter, "").await.verdict, Verdict::Pass);
    }

    #[tokio::test]
    async fn a_filter_that_never_reads_stdin_still_gets_a_verdict() {
        let dir = TempDir::new().unwrap();
        let filter = script(&dir, "echo 'nothing to see'");
        let outcome = run(&filter, &"x".repeat(1 << 20)).await;
        assert_eq!(outcome.verdict, Verdict::Rewritten("nothing to see".into()));
    }

    #[tokio::test]
    async fn stdin_is_written_while_stdout_and_a_long_note_are_read() {
        let dir = TempDir::new().unwrap();
        let filter = script(
            &dir,
            "head -c 40000 /dev/zero | tr '\\0' x\nhead -c 200000 /dev/zero | tr '\\0' y >&2\ncat >/dev/null",
        );
        let outcome = run(&filter, &"x".repeat(1 << 20)).await;
        assert_eq!(outcome.verdict, Verdict::Rewritten("x".repeat(40_000)));
        assert_eq!(outcome.stderr, "y".repeat(MAX_OUTPUT_BYTES));
    }

    #[tokio::test]
    async fn the_context_is_in_env_vars_and_what_does_not_apply_is_unset() {
        let dir = TempDir::new().unwrap();
        let filter = script(&dir, "cat >/dev/null\nenv | grep '^LOURO_' | sort");
        let context = Context {
            kind: "react",
            nick: Some("maria".into()),
            msgid: Some("4y8kq7j2c9mnz".into()),
            self_nick: "louro".into(),
            self_account: Some("louro".into()),
            ..Context::default()
        };
        let outcome = filter.run("👍", &context).await;
        let env = "\
LOURO_FILTER_VERSION=1
LOURO_KIND=react
LOURO_MSGID=4y8kq7j2c9mnz
LOURO_NICK=maria
LOURO_SELF_ACCOUNT=louro
LOURO_SELF_NICK=louro
LOURO_SERVER=irc.example.org";
        assert_eq!(outcome.verdict, Verdict::Rewritten(env.into()));
    }

    #[tokio::test]
    async fn at_most_four_filters_run_at_once() {
        let dir = TempDir::new().unwrap();
        let running = dir.path().display();
        let filter = Arc::new(script(
            &dir,
            &format!(
                "mkdir '{running}/running.'$$\nls -d '{running}'/running.* | wc -l\nsleep 0.2\nrmdir '{running}/running.'$$"
            ),
        ));
        let mut runs = JoinSet::new();
        for _ in 0..8 {
            let filter = Arc::clone(&filter);
            runs.spawn(async move { filter.run("hi", &message()).await.verdict });
        }
        while let Some(verdict) = runs.join_next().await {
            let Verdict::Rewritten(count) = verdict.unwrap() else {
                panic!("the filter failed");
            };
            let count: usize = count.trim().parse().unwrap();
            assert!(count <= MAX_RUNNING, "{count} filters ran at once");
        }
    }
}
