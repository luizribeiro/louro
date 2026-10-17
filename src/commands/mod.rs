use std::path::Path;
use std::process::ExitCode;

use clap::Subcommand;

use crate::output;
use crate::socket::{self, Op};

pub mod connect;
mod history;
mod send;
mod wait;

#[derive(Subcommand)]
pub enum Command {
    /// Block until matching events arrive
    Wait(wait::Args),
    /// Send a message, notice or action
    Send(send::Args),
    /// Earlier chat in a channel or DM, from the server
    History(history::Args),
    /// Start the daemon (operator only)
    Connect(connect::Args),
    #[command(flatten)]
    Request(Op),
}

pub async fn dispatch(command: Command, socket: Option<&Path>) -> ExitCode {
    match command {
        Command::Wait(args) => args.run(socket).await,
        Command::Send(args) => args.run(socket).await,
        Command::History(args) => args.run(socket).await,
        Command::Connect(args) => args.run(socket).await,
        Command::Request(op) => output::emit(socket::request(socket, op).await),
    }
}
