//! `louro connect`: the operator starts the daemon that holds the IRC connection.

use std::env;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{self, ExitCode, Stdio};

use serde::Deserialize;
use serde_json::Value;

use crate::config::{self, Config, Server};
use crate::daemon;
use crate::error::{Code, Error, Result};
use crate::irc::Secret;
use crate::output;
use crate::password::{self, Source};
use crate::socket::{Connection, Op};

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
    /// Run in this terminal instead of in the background
    #[arg(long)]
    pub foreground: bool,
    /// Read the password from stdin; how `connect` hands it to its background daemon
    #[arg(long, hide = true)]
    pub password_stdin: bool,
}

impl Args {
    pub async fn run(self, socket: Option<&Path>) -> ExitCode {
        let config = match config::load(self.config.as_deref(), socket) {
            Ok(config) => config,
            Err(err) => return output::emit::<()>(Err(err)),
        };
        if is_running(config.server.socket.as_deref()).await {
            return output::emit::<()>(Err(daemon::already_running()));
        }
        if self.foreground {
            foreground(self.password_stdin, config).await
        } else {
            output::emit(background(&config))
        }
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

async fn is_running(socket: Option<&Path>) -> bool {
    let Ok(mut louro) = Connection::connect(socket).await else {
        return false;
    };
    louro.request(Op::Status {}).await.is_ok()
}

/// Re-runs this binary as `connect --foreground` in its own process group,
/// hands it the password on stdin, and relays the line it prints once ready.
fn background(config: &Config) -> Result<Value> {
    let password = read_password(false, &config.server)?;
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

async fn foreground(password_stdin: bool, config: Config) -> ExitCode {
    let password = match read_password(password_stdin, &config.server) {
        Ok(password) => password,
        Err(err) => return output::emit::<()>(Err(err)),
    };
    let daemon = match daemon::start(config, password).await {
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
