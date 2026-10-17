use std::io;
use std::path::Path;
use std::process::ExitCode;

use serde_json::Value;

use crate::error::{Code, Error, Result};
use crate::irc::Kind;
use crate::output;
use crate::socket::{self, Op};

#[derive(clap::Args)]
pub struct Args {
    /// Channel or nick to send to
    pub target: String,
    /// Text to send, or `-` to read it from stdin (multi-line)
    pub text: String,
    /// Send as a NOTICE instead of a message
    #[arg(long, conflicts_with = "action")]
    pub notice: bool,
    /// Send as a CTCP ACTION ("/me ...")
    #[arg(long)]
    pub action: bool,
}

impl Args {
    pub async fn run(self, socket: Option<&Path>) -> ExitCode {
        output::emit(self.send(socket).await)
    }

    async fn send(self, socket: Option<&Path>) -> Result<Value> {
        let kind = match (self.notice, self.action) {
            (true, _) => Kind::Notice,
            (_, true) => Kind::Action,
            (false, false) => Kind::Message,
        };
        let text = read_text(self.text)?;
        socket::request(
            socket,
            Op::Send {
                target: self.target,
                text,
                kind,
            },
        )
        .await
    }
}

fn read_text(text: String) -> Result<String> {
    if text != "-" {
        return Ok(text);
    }
    io::read_to_string(io::stdin()).map_err(|err| {
        Error::new(
            Code::Usage,
            format!("could not read text from stdin: {err}"),
        )
    })
}
