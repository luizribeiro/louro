use std::path::Path;
use std::process::ExitCode;

use crate::output;
use crate::socket::{self, Op};

#[derive(clap::Args)]
pub struct Args {}

impl Args {
    pub async fn run(self, socket: Option<&Path>) -> ExitCode {
        output::emit(socket::request(socket, Op::Quit {}).await)
    }
}
