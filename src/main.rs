use std::process::ExitCode;

use clap::Parser;

use louro::cli::{Cli, Command};
use louro::error::{Code, Error};
use louro::output;

fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(err) if !err.use_stderr() => err.exit(),
        Err(err) => {
            return output::emit::<()>(Err(Error::new(Code::Usage, err.render().to_string())));
        }
    };

    match cli.command {
        Command::Connect => output::emit::<()>(Err(Error::new(
            Code::Internal,
            "connect is not wired to papo yet",
        ))),
        Command::Status | Command::Wait(_) | Command::Send(_) | Command::Quit => {
            output::emit::<()>(Err(Error::new(
                Code::Internal,
                "not wired to the daemon yet",
            )))
        }
    }
}
