use std::io;
use std::path::Path;
use std::process::ExitCode;

use clap::Parser;
use serde_json::Value;

mod cli;
mod connect;
mod daemon;
mod error;
mod grants;
mod inbox;
mod irc;
mod output;
mod password;
mod socket;

use cli::{Cli, Command, PRIMER};
use error::{Code, Error, Result};
use irc::Kind;
use socket::{Connection, Op, SendArgs};

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(err) if !err.use_stderr() => err.exit(),
        Err(err) => {
            return output::emit::<()>(Err(Error::new(Code::Usage, err.render().to_string())));
        }
    };

    let Some(command) = cli.command else {
        print!("{PRIMER}");
        return ExitCode::SUCCESS;
    };

    let socket = cli.socket.as_deref();
    match command {
        Command::Connect(args) => connect::run(args, socket).await,
        Command::Status => output::emit(request(socket, Op::Status {}).await),
        Command::Quit => output::emit(request(socket, Op::Quit {}).await),
        Command::Send(args) => output::emit(send(socket, args).await),
        Command::Wait(args) => wait(socket, args).await,
    }
}

async fn request(socket: Option<&Path>, op: Op) -> Result<Value> {
    Connection::connect(socket).await?.request(op).await
}

/// Acks only after printing, so the daemon never drops events the agent didn't see.
async fn wait(socket: Option<&Path>, args: cli::WaitArgs) -> ExitCode {
    let mut connection = match Connection::connect(socket).await {
        Ok(connection) => connection,
        Err(err) => return output::emit::<()>(Err(err)),
    };
    let result = connection.request(Op::Wait(args)).await;
    let printed_ok = result.is_ok();
    let exit_code = output::emit(result);
    if printed_ok {
        let _ = connection.ack().await;
    }
    exit_code
}

async fn send(socket: Option<&Path>, args: cli::SendArgs) -> Result<Value> {
    let kind = match (args.notice, args.action) {
        (true, _) => Kind::Notice,
        (_, true) => Kind::Action,
        (false, false) => Kind::Message,
    };
    let text = read_text(args.text)?;
    request(
        socket,
        Op::Send(SendArgs {
            target: args.target,
            text,
            kind,
        }),
    )
    .await
}

fn read_text(text: String) -> Result<String> {
    if text != "-" {
        return Ok(text);
    }
    io::read_to_string(io::stdin()).map_err(|err| {
        Error::new(
            Code::Usage,
            format!("could not read text from stdin: {err}"),
        )
    })
}
