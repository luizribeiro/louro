use std::process::ExitCode;

use serde::Serialize;

use crate::error::{Error, Result};

#[derive(Serialize)]
struct ErrorBody<'a> {
    error: &'a Error,
}

pub fn emit<T: Serialize>(result: Result<T>) -> ExitCode {
    println!("{}", render(&result));
    match result {
        Ok(_) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("louro: {error}");
            ExitCode::from(error.code.exit_code())
        }
    }
}

fn render<T: Serialize>(result: &Result<T>) -> String {
    let rendered = match result {
        Ok(body) => serde_json::to_string(body),
        Err(error) => serde_json::to_string(&ErrorBody { error }),
    };
    rendered.expect("louro output must serialize to JSON")
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use crate::error::Code;

    use super::*;

    #[derive(Serialize)]
    struct Sent {
        target: &'static str,
        msgid: &'static str,
        threaded: bool,
        lines: u32,
    }

    #[test]
    fn renders_a_successful_result_as_the_bare_object() {
        let sent = Sent {
            target: "#bots",
            msgid: "c2m8w1u0pq7rs",
            threaded: true,
            lines: 1,
        };
        assert_eq!(
            render(&Ok(sent)),
            r##"{"target":"#bots","msgid":"c2m8w1u0pq7rs","threaded":true,"lines":1}"##
        );
    }

    #[test]
    fn renders_an_error_with_a_hint() {
        let not_allowed = Error::new(Code::NotAllowed, "#lounge is not in the channel allowlist")
            .with_hint("don't retry; the operator set this");
        assert_eq!(
            render::<Value>(&Err(not_allowed)),
            r##"{"error":{"code":"not_allowed","message":"#lounge is not in the channel allowlist","hint":"don't retry; the operator set this"}}"##
        );
    }
}
