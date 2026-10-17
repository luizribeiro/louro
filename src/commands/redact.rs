use std::path::Path;
use std::process::ExitCode;

use crate::output;
use crate::socket::{self, Op};

#[derive(clap::Args)]
pub struct Args {
    /// Channel or nick the message was sent to
    pub target: String,
    /// The msgid `louro send` returned
    pub msgid: String,
    /// Why, shown to clients that support redaction
    #[arg(long, default_value = "")]
    pub reason: String,
}

impl Args {
    pub async fn run(self, socket: Option<&Path>) -> ExitCode {
        let op = Op::Redact {
            target: self.target,
            msgid: self.msgid,
            reason: self.reason,
        };
        output::emit(socket::request(socket, op).await)
    }
}
