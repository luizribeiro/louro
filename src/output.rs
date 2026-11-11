use std::io::{self, Write};
use std::process::ExitCode;

use serde::Serialize;
use serde_json::{Value, json};

use crate::error::{Error, Result};
use crate::irc;

pub fn emit<T: Serialize>(result: Result<T>) -> ExitCode {
    match result {
        Ok(body) => {
            println!("{}", with_now(json!(body)));
            ExitCode::SUCCESS
        }
        Err(error) => {
            println!("{}", json!({ "error": error }));
            let _ = write_error_line(io::stderr(), &error);
            ExitCode::from(error.code.exit_code())
        }
    }
}

fn write_error_line(mut out: impl Write, error: &Error) -> io::Result<()> {
    out.write_all(format!("louro: {error}\n").as_bytes())
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
    use crate::error::Code;

    #[derive(Default)]
    struct Writes(Vec<Vec<u8>>);

    impl Write for Writes {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.push(buf.to_vec());
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn the_error_line_goes_out_in_one_write_so_other_output_cant_split_it() {
        let error = Error::new(Code::NotAllowed, "irc.dm.send does not grant mallory")
            .with_hint("don't retry; the operator chose this");
        let mut writes = Writes::default();
        write_error_line(&mut writes, &error).unwrap();
        assert_eq!(
            writes.0,
            [b"louro: irc.dm.send does not grant mallory (don't retry; the operator chose this)\n".to_vec()]
        );
    }
}
