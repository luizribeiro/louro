use std::path::Path;
use std::process::ExitCode;

use clap::Subcommand;

pub mod connect;
mod history;
mod names;
mod quit;
mod react;
mod redact;
mod send;
mod status;
mod typing;
mod wait;

#[derive(Subcommand)]
pub enum Command {
    /// Connection state, nick, channels, and what the bot may do (grants)
    Status(status::Args),
    /// Block until matching events arrive
    Wait(wait::Args),
    /// Send a message, notice or action
    Send(send::Args),
    /// React to a message with an emoji
    React(react::Args),
    /// Delete a message the bot sent
    Redact(redact::Args),
    /// Show that the bot is typing
    Typing(typing::Args),
    /// Earlier chat in a channel or DM, from the server
    History(history::Args),
    /// Members of a channel the bot is in
    Names(names::Args),
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
        Command::React(args) => args.run(socket).await,
        Command::Typing(args) => args.run(socket).await,
        Command::Redact(args) => args.run(socket).await,
        Command::History(args) => args.run(socket).await,
        Command::Names(args) => args.run(socket).await,
        Command::Quit(args) => args.run(socket).await,
        Command::Connect(args) => args.run(socket).await,
    }
}
