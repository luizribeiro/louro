//! `louro connect`: the operator starts the daemon that holds the IRC connection.

use std::env;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{self, ExitCode, Stdio};

use serde::Deserialize;
use serde_json::Value;

use crate::daemon;
use crate::error::{Code, Error, Result};
use crate::output;
use crate::socket::{Connection, Op};

#[derive(Deserialize)]
#[serde(untagged)]
enum ReadyLine {
    Failed { error: Error },
    Connected(Value),
}

#[expect(clippy::struct_excessive_bools, reason = "each is an independent flag")]
#[derive(clap::Args)]
pub struct Args {
    /// IRC server host name
    pub server: String,
    /// The bot's nick
    #[arg(long)]
    pub nick: String,
    /// Port [default: 6697, or 6667 with --plaintext]
    #[arg(long)]
    pub port: Option<u16>,
    /// Connect without TLS
    #[arg(long)]
    pub plaintext: bool,
    /// SASL account; prompts for its password on the terminal
    #[arg(long, value_name = "ACCOUNT")]
    pub sasl_user: Option<String>,
    /// Read the SASL password from a file only you can read (chmod 600)
    #[arg(long, value_name = "PATH")]
    pub password_file: Option<PathBuf>,
    /// Channel to join and allow sends to; repeatable
    #[arg(long = "join", value_name = "CHANNEL")]
    pub joins: Vec<String>,
    /// Receive and send direct messages
    #[arg(long)]
    pub allow_dm: bool,
    /// Run in this terminal instead of in the background
    #[arg(long)]
    pub foreground: bool,
    /// Read the password from stdin; how `connect` hands it to its background daemon
    #[arg(long, hide = true)]
    pub password_stdin: bool,
}

impl Args {
    pub fn port(&self) -> u16 {
        self.port
            .unwrap_or(if self.plaintext { 6667 } else { 6697 })
    }

    pub async fn run(self, socket: Option<&Path>) -> ExitCode {
        if is_running(socket).await {
            return output::emit::<()>(Err(Error::new(
                Code::SessionExists,
                "louro is already running",
            )
            .with_hint("run `louro quit` first")));
        }
        if self.foreground {
            foreground(self, socket).await
        } else {
            output::emit(background(&self))
        }
    }
}

async fn is_running(socket: Option<&Path>) -> bool {
    let Ok(mut louro) = Connection::connect(socket).await else {
        return false;
    };
    louro.request(Op::Status {}).await.is_ok()
}

/// Re-runs this binary as `connect --foreground` in its own process group,
/// hands it the password on stdin, and relays the line it prints once ready.
fn background(args: &Args) -> Result<Value> {
    let password = daemon::read_password(args)?;
    let mut daemon = process::Command::new(env::current_exe().map_err(|err| cannot_start(&err))?)
        .args(env::args_os().skip(1))
        .arg("--foreground")
        .args(password.is_some().then_some("--password-stdin"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map_err(|err| cannot_start(&err))?;
    if let (Some(mut stdin), Some(password)) = (daemon.stdin.take(), password) {
        let _ = stdin.write_all(password.expose().as_bytes());
    }
    let mut line = String::new();
    if let Some(stdout) = daemon.stdout.take() {
        let _ = BufReader::new(stdout).read_line(&mut line);
    }
    match serde_json::from_str(&line) {
        Ok(ReadyLine::Failed { error }) => Err(error),
        Ok(ReadyLine::Connected(connected)) => Ok(connected),
        Err(_) => Err(
            Error::new(Code::ConnectFailed, "the daemon exited before connecting")
                .with_hint("run it with --foreground to see why"),
        ),
    }
}

fn cannot_start(err: &io::Error) -> Error {
    Error::new(Code::Internal, format!("can't start the daemon: {err}"))
}

async fn foreground(args: Args, socket: Option<&Path>) -> ExitCode {
    let daemon = match daemon::start(args, socket).await {
        Ok(daemon) => daemon,
        Err(err) => return output::emit::<()>(Err(err)),
    };
    match daemon.await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("louro: {err}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[derive(Parser)]
    struct Test {
        #[command(flatten)]
        args: Args,
    }

    fn parse(flags: &[&str]) -> Args {
        Test::try_parse_from([&["connect", "127.0.0.1", "--nick", "louro"][..], flags].concat())
            .unwrap()
            .args
    }

    #[test]
    fn connect_port_defaults_to_tls_or_plaintext() {
        for (flags, port) in [(&[][..], 6697), (&["--plaintext"], 6667)] {
            assert_eq!(parse(flags).port(), port);
        }
    }
}
