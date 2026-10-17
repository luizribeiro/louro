use std::path::Path;
use std::process::ExitCode;

use clap::Subcommand;

pub mod connect;
mod quit;
mod send;
mod status;
mod wait;

#[derive(Subcommand)]
pub enum Command {
    /// Connection state, nick, channels, and what the bot may do (grants)
    Status(status::Args),
    /// Block until matching events arrive
    Wait(wait::Args),
    /// Send a message, notice or action
    Send(send::Args),
    /// Disconnect and stop the daemon
    Quit(quit::Args),
    /// Start the daemon (operator only)
    Connect(connect::Args),
}

pub async fn dispatch(command: Command, socket: Option<&Path>) -> ExitCode {
    match command {
        Command::Status(args) => args.run(socket).await,
        Command::Wait(args) => args.run(socket).await,
        Command::Send(args) => args.run(socket).await,
        Command::Quit(args) => args.run(socket).await,
        Command::Connect(args) => args.run(socket).await,
    }
}
