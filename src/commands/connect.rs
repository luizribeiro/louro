//! `louro connect`: the operator starts the daemon that holds the IRC connection.

use std::env;
use std::fmt::Display;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::Duration;

use rustix::process::{self, Pid, Signal};
use serde::Deserialize;
use serde_json::Value;

use crate::config::{self, Server};
use crate::daemon;
use crate::error::{Code, Error, Result};
use crate::irc::Secret;
use crate::output;
use crate::password::{self, Source};
use crate::socket::Connection;

#[derive(Deserialize)]
#[serde(untagged)]
enum ReadyLine {
    Failed { error: Error },
    Connected(Value),
}

#[derive(clap::Args)]
pub struct Args {
    /// Config file [default: louro/louro.toml in the XDG config dir, ~/.config]
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,
    /// Unix socket to listen on [default: `socket` in louro.toml, else `LOURO_SOCKET`, else one in
    /// the runtime dir]
    #[arg(long, value_name = "PATH")]
    pub socket: Option<PathBuf>,
    /// Run in this terminal instead of in the background
    #[arg(long)]
    pub foreground: bool,
    /// Stop the louro running on this socket (SIGTERM), once the config and password are read,
    /// then start this one in its place
    #[arg(long)]
    pub replace: bool,
    /// Read the password from stdin; how `connect` hands it to its background daemon
    #[arg(long, hide = true)]
    pub password_stdin: bool,
}

impl Args {
    pub async fn run(self) -> ExitCode {
        self.connect()
            .await
            .unwrap_or_else(|err| output::emit::<()>(Err(err)))
    }

    async fn connect(self) -> Result<ExitCode> {
        let config = config::load(self.config.as_deref(), self.socket.as_deref())?;
        if let Some(filter) = &config.filter {
            filter.check_program()?;
        }
        let socket = config.server.socket.as_deref();
        let running = Connection::connect(socket).await.ok();
        if running.is_some() && !self.replace {
            return Err(Error::new(Code::SessionExists, "louro is already running")
                .with_hint("run `louro connect --replace`, or `louro quit` first"));
        }
        let password = read_password(self.password_stdin, &config.server)?;
        if let Some(running) = running {
            stop(&running)?;
            wait_until_stopped(socket).await?;
        }
        if !self.foreground {
            return Ok(output::emit(background(password)));
        }
        if let Err(err) = daemon::start(config, password).await?.await {
            eprintln!("louro: {err}");
            return Ok(ExitCode::FAILURE);
        }
        Ok(ExitCode::SUCCESS)
    }
}

/// The background daemon gets the password on stdin, so only `connect` reads the file or prompts.
fn read_password(password_stdin: bool, server: &Server) -> Result<Option<Secret>> {
    let source = if password_stdin {
        Some(Source::Stdin)
    } else if let Some(path) = &server.password_file {
        Some(Source::File(path.clone()))
    } else {
        server
            .sasl_username()
            .map(|user| Source::Tty(user.to_owned()))
    };
    source.map(|source| password::read(&source)).transpose()
}

/// Works on a daemon of any socket version: it only needs the connection's peer credentials.
fn stop(running: &Connection) -> Result<()> {
    let peer = running.peer_cred().map_err(cannot_replace)?;
    if peer.uid() != process::getuid().as_raw() {
        return Err(cannot_replace("it runs as another user"));
    }
    let pid = peer
        .pid()
        .and_then(Pid::from_raw)
        .ok_or_else(|| cannot_replace("the socket doesn't say which process it is"))?;
    process::kill_process(pid, Signal::TERM).map_err(cannot_replace)
}

/// The daemon removes its socket last, once its IRC connection has quit.
async fn wait_until_stopped(socket: Option<&Path>) -> Result<()> {
    for _ in 0..100 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if Connection::connect(socket).await.is_err() {
            return Ok(());
        }
    }
    Err(cannot_replace("it didn't stop within 10s"))
}

fn cannot_replace(why: impl Display) -> Error {
    Error::new(
        Code::SessionExists,
        format!("can't replace the running louro: {why}"),
    )
}

/// Re-runs this binary as `connect --foreground` in its own process group, without `--replace`
/// since that already happened, hands it the password on stdin, and relays the line it prints once
/// ready.
fn background(password: Option<Secret>) -> Result<Value> {
    let mut daemon = Command::new(env::current_exe().map_err(|err| cannot_start(&err))?)
        .args(env::args_os().skip(1).filter(|arg| arg != "--replace"))
        .arg("--foreground")
        .args(password.is_some().then_some("--password-stdin"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map_err(|err| cannot_start(&err))?;
    if let (Some(mut stdin), Some(password)) = (daemon.stdin.take(), password) {
        let _ = stdin.write_all(password.expose_bytes());
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
