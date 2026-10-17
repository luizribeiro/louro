use std::path::Path;
use std::process::ExitCode;

use clap::Subcommand;

use crate::output;
use crate::socket::{self, Connection, Op};

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

pub async fn dispatch(command: Command, socket: Option<&Path>) -> ExitCode {
    match command {
        Command::Send(args) => args.run(socket).await,
        Command::Connect(args) => args.run(socket).await,
        Command::Request(op @ Op::Wait { .. }) => wait(socket, op).await,
        Command::Request(op) => output::emit(socket::request(socket, op).await),
    }
}

/// Acks only after printing, so the daemon never drops events the agent didn't see.
async fn wait(socket: Option<&Path>, op: Op) -> ExitCode {
    let mut connection = match Connection::connect(socket).await {
        Ok(connection) => connection,
        Err(err) => return output::emit::<()>(Err(err)),
    };
    let result = connection.request(op).await;
    let printed_ok = result.is_ok();
    let exit_code = output::emit(result);
    if printed_ok {
        let _ = connection.ack().await;
    }
    exit_code
}
