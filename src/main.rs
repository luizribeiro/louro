use std::io::Read;
use std::path::Path;
use std::process::ExitCode;

use clap::Parser;
use serde_json::Value;

use louro::cli::{self, Cli, Command, PRIMER};
use louro::client::Connection;
use louro::error::{Code, Error, Result};
use louro::output;
use louro::protocol::{Kind, Op, SendArgs, WaitArgs};

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

    match command {
        Command::Connect(_) => output::emit::<()>(Err(Error::new(
            Code::Internal,
            "connect is not wired to papo yet",
        ))),
        Command::Status => output::emit(request(cli.socket.as_deref(), Op::Status {}).await),
        Command::Quit => output::emit(request(cli.socket.as_deref(), Op::Quit {}).await),
        Command::Send(args) => output::emit(send(cli.socket.as_deref(), args).await),
        Command::Wait(args) => wait(cli.socket.as_deref(), args).await,
    }
}

async fn request(socket: Option<&Path>, op: Op) -> Result<Value> {
    Connection::connect(socket).await?.request(op).await
}

/// Prints the result before acking: the daemon only advances the consumer
/// cursor once it knows the CLI showed the agent what arrived (design §4.4),
/// so the ack must follow the print, not race it.
async fn wait(socket: Option<&Path>, args: cli::WaitArgs) -> ExitCode {
    let mut connection = match Connection::connect(socket).await {
        Ok(connection) => connection,
        Err(err) => return output::emit::<()>(Err(err)),
    };
    let op = Op::Wait(WaitArgs {
        timeout_secs: args.timeout.as_secs(),
        max: args.max,
    });
    let result = connection.request(op).await;
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
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf).map_err(|err| {
        Error::new(
            Code::Usage,
            format!("could not read text from stdin: {err}"),
        )
    })?;
    Ok(buf)
}
