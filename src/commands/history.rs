use std::path::Path;
use std::process::ExitCode;

use crate::output;
use crate::socket::{self, Op};

#[derive(clap::Args)]
pub struct Args {
    /// Channel, or the nick of a DM conversation
    pub target: String,
    /// How many messages to return, at most 100
    #[arg(long, default_value = "20", value_name = "N")]
    pub latest: usize,
    /// Only messages before this msgid or RFC 3339 time
    #[arg(long, value_name = "MSGID|TIME")]
    pub before: Option<String>,
    /// Only messages after this msgid or RFC 3339 time
    #[arg(long, value_name = "MSGID|TIME")]
    pub after: Option<String>,
}

impl Args {
    pub async fn run(self, socket: Option<&Path>) -> ExitCode {
        let op = Op::History {
            target: self.target,
            limit: self.latest,
            before: self.before,
            after: self.after,
        };
        output::emit(socket::request(socket, op).await)
    }
}
