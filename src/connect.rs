//! `louro connect`: the operator starts the daemon that holds the IRC connection.

use std::fs::DirBuilder;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serde::Serialize;
use tokio::net::UnixListener;
use tokio::sync::mpsc;

use crate::cli::ConnectArgs;
use crate::client;
use crate::daemon::{self, Config, Incoming};
use crate::error::{Code, Error, Result};
use crate::output;
use crate::papo::{self, ConnectOptions, PapoIrc};
use crate::password::{self, Source};

/// The one line `connect` prints once the bot is registered and the socket is up.
#[derive(Serialize)]
struct Connected {
    server: String,
    nick: String,
    channels: Vec<String>,
    socket: PathBuf,
}

struct Ready {
    connected: Connected,
    config: Config,
    irc: PapoIrc,
    incoming: mpsc::Receiver<Incoming>,
    listener: UnixListener,
}

/// Connects, prints the result line, then serves the socket until `quit`.
pub async fn foreground(args: ConnectArgs, socket: Option<&Path>) -> ExitCode {
    let ready = match start(args, socket).await {
        Ok(ready) => ready,
        Err(err) => return output::emit::<()>(Err(err)),
    };
    output::emit(Ok(&ready.connected));
    let Ready {
        connected,
        config,
        irc,
        incoming,
        listener,
    } = ready;
    match daemon::run(config, irc, incoming, listener, &connected.socket).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("louro: {err}");
            ExitCode::FAILURE
        }
    }
}

async fn start(args: ConnectArgs, socket: Option<&Path>) -> Result<Ready> {
    let socket = client::resolve_socket_path(socket)?;
    let password = read_password(&args)?;
    create_runtime_dir(&socket)?;
    let (irc, incoming) = papo::connect(ConnectOptions {
        server: args.server.clone(),
        port: args.port(),
        tls: !args.plaintext,
        nick: args.nick.clone(),
        sasl_user: args.sasl_user.clone(),
        password: password.clone(),
        channels: args.joins.clone(),
    })
    .await?;
    let listener = daemon::bind(&socket).map_err(|err| cannot_listen(&socket, &err))?;
    Ok(Ready {
        connected: Connected {
            server: format!("{}:{}", args.server, args.port()),
            nick: args.nick,
            channels: args.joins.clone(),
            socket,
        },
        config: Config {
            channels: args.joins,
            allow_dm: args.allow_dm,
            password,
        },
        irc,
        incoming,
        listener,
    })
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

fn cannot_listen(socket: &Path, err: &std::io::Error) -> Error {
    Error::new(
        Code::Internal,
        format!("can't listen on {}: {err}", socket.display()),
    )
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;
    use crate::cli::{Cli, Command};

    fn source(flags: &[&str]) -> Option<Source> {
        let required = ["louro", "connect", "127.0.0.1", "--nick", "louro"];
        let cli = Cli::try_parse_from(required.iter().chain(flags)).unwrap();
        let Some(Command::Connect(args)) = cli.command else {
            panic!("expected connect");
        };
        password_source(&args)
    }

    #[test]
    fn asks_for_a_password_only_for_sasl_and_prefers_the_parents_pipe() {
        assert!(source(&[]).is_none());
        assert!(
            matches!(source(&["--sasl-user", "louro"]), Some(Source::Tty(user)) if user == "louro")
        );
        let file = [
            "--sasl-user",
            "louro",
            "--password-file",
            "/run/creds/louro",
        ];
        assert!(matches!(source(&file), Some(Source::File(_))));
        let piped = [&file[..], &["--password-stdin"]].concat();
        assert!(matches!(source(&piped), Some(Source::Stdin)));
    }
}
