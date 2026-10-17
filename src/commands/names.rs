use std::path::Path;
use std::process::ExitCode;

use crate::output;
use crate::socket::{self, Op};

#[derive(clap::Args)]
pub struct Args {
    /// A channel the bot is in
    pub channel: String,
}

impl Args {
    pub async fn run(self, socket: Option<&Path>) -> ExitCode {
        let op = Op::Names {
            channel: self.channel,
        };
        output::emit(socket::request(socket, op).await)
    }
}
