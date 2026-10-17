use std::process::ExitCode;

use serde::Serialize;
use serde_json::json;

use crate::error::Result;

pub fn emit<T: Serialize>(result: Result<T>) -> ExitCode {
    match result {
        Ok(body) => {
            println!("{}", json!(body));
            ExitCode::SUCCESS
        }
        Err(error) => {
            println!("{}", json!({ "error": error }));
            eprintln!("louro: {error}");
            ExitCode::from(error.code.exit_code())
        }
    }
}
