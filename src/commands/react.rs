use std::path::Path;
use std::process::ExitCode;

use crate::output;
use crate::socket::{self, Op};

#[derive(clap::Args)]
pub struct Args {
    /// Channel or nick the message was sent to
    pub target: String,
    /// The msgid of the message to react to
    pub msgid: String,
    /// A short emoji or word, up to 32 characters
    pub reaction: String,
    /// Remove this reaction instead of adding it
    #[arg(long)]
    pub remove: bool,
}

impl Args {
    pub async fn run(self, socket: Option<&Path>) -> ExitCode {
        let op = Op::React {
            target: self.target,
            msgid: self.msgid,
            reaction: self.reaction,
            remove: self.remove,
        };
        output::emit(socket::request(socket, op).await)
    }
}
