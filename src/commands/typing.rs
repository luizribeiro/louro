use std::path::Path;
use std::process::ExitCode;

use crate::irc::Typing;
use crate::output;
use crate::socket::{self, Op};

#[derive(clap::Args)]
pub struct Args {
    /// Channel or nick to show typing to
    pub target: String,
    /// `active` while composing, `paused`, or `done` once finished
    pub state: Typing,
}

impl Args {
    pub async fn run(self, socket: Option<&Path>) -> ExitCode {
        let op = Op::Typing {
            target: self.target,
            state: self.state,
        };
        output::emit(socket::request(socket, op).await)
    }
}
