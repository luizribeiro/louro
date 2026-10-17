use std::process::ExitCode;

use clap::Parser;

mod cli;
mod commands;
mod connect;
mod daemon;
mod error;
mod grants;
mod inbox;
mod irc;
mod output;
mod password;
mod socket;
mod text;

use cli::{Cli, PRIMER};
use error::{Code, Error};

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

    commands::dispatch(command, cli.socket.as_deref()).await
}
