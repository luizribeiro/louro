//! `louro connect`: the operator starts the daemon that holds the IRC connection.

use std::env;
use std::fs::DirBuilder;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{self, ExitCode, Stdio};

use serde::Deserialize;
use serde_json::{Value, json};

use crate::cli::ConnectArgs;
use crate::client::{self, Connection};
use crate::daemon::{self, Config};
use crate::error::{Code, Error, Result};
use crate::output;
use crate::papo;
use crate::password::{self, Source};
use crate::protocol::Op;

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
    let password = read_password(args)?;
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
        let _ = stdin.write_all(password.as_bytes());
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
    let daemon = match start(args, socket).await {
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

/// Connects and listens, prints the line `background` waits for, and returns the daemon to run.
async fn start(
    args: ConnectArgs,
    socket: Option<&Path>,
) -> Result<impl Future<Output = io::Result<()>>> {
    let socket = client::resolve_socket_path(socket)?;
    let password = read_password(&args)?;
    create_runtime_dir(&socket)?;
    let (irc, incoming) = papo::connect(&args, password.clone()).await?;
    let listener = daemon::bind(&socket).map_err(|err| cannot_listen(&socket, &err))?;
    output::emit(Ok(json!({
        "server": format!("{}:{}", args.server, args.port()),
        "nick": args.nick,
        "channels": args.joins,
        "socket": socket,
    })));
    let config = Config {
        channels: args.joins,
        allow_dm: args.allow_dm,
        password,
    };
    Ok(async move { daemon::run(config, irc, incoming, listener, &socket).await })
}

fn read_password(args: &ConnectArgs) -> Result<Option<String>> {
    password_source(args)
        .map(|source| password::read(&source))
        .transpose()
}

/// A password is only wanted for SASL.
fn password_source(args: &ConnectArgs) -> Option<Source> {
    if args.password_stdin {
        return Some(Source::Stdin);
    }
    if let Some(path) = &args.password_file {
        return Some(Source::File(path.clone()));
    }
    let user = args.sasl_user.as_ref()?;
    Some(Source::Tty(user.clone()))
}

fn create_runtime_dir(socket: &Path) -> Result<()> {
    let Some(dir) = socket.parent() else {
        return Ok(());
    };
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(|err| cannot_listen(socket, &err))
}

fn cannot_listen(socket: &Path, err: &io::Error) -> Error {
    Error::new(
        Code::Internal,
        format!("can't listen on {}: {err}", socket.display()),
    )
}
