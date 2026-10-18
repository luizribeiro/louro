use std::process::ExitCode;

use serde::Serialize;
use serde_json::{Value, json};

use crate::error::Result;
use crate::irc;

pub fn emit<T: Serialize>(result: Result<T>) -> ExitCode {
    match result {
        Ok(body) => {
            println!("{}", with_now(json!(body)));
            ExitCode::SUCCESS
        }
        Err(error) => {
            println!("{}", json!({ "error": error }));
            eprintln!("louro: {error}");
            ExitCode::from(error.code.exit_code())
        }
    }
}

/// An agent can't tell the time, so each result says it, to judge how old events are.
fn with_now(mut body: Value) -> Value {
    if let Some(fields) = body.as_object_mut() {
        fields.insert("now".into(), irc::now().into());
    }
    body
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_result_says_the_current_time() {
        let result = with_now(json!({"sent": true, "target": "#bots"}));
        assert_eq!(result["sent"], true);
        assert!(result["now"].as_str().unwrap().ends_with('Z'), "{result}");
    }
}
