//! `louro connect`: the operator starts the daemon that holds the IRC connection.

use std::env;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{self, ExitCode, Stdio};

use serde::Deserialize;
use serde_json::Value;

use crate::cli::ConnectArgs;
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

pub async fn run(args: ConnectArgs, socket: Option<&Path>) -> ExitCode {
    if is_running(socket).await {
        return output::emit::<()>(Err(Error::new(
            Code::SessionExists,
            "louro is already running",
        )
        .with_hint("run `louro quit` first")));
    }
    if args.foreground {
        foreground(args, socket).await
    } else {
        output::emit(background(&args))
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
fn background(args: &ConnectArgs) -> Result<Value> {
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

async fn foreground(args: ConnectArgs, socket: Option<&Path>) -> ExitCode {
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
