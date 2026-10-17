use assert_cmd::Command;
use predicates::prelude::*;

fn louro() -> Command {
    Command::cargo_bin("louro").unwrap()
}

#[test]
fn a_bad_flag_is_a_json_usage_error_with_exit_code_two() {
    louro()
        .arg("send")
        .arg("#bots")
        .arg("hi")
        .arg("--not-a-flag")
        .assert()
        .failure()
        .code(2)
        .stdout(predicate::str::contains(r#""code":"usage""#));
}

#[test]
fn a_missing_subcommand_is_a_json_usage_error_with_exit_code_two() {
    louro()
        .assert()
        .failure()
        .code(2)
        .stdout(predicate::str::contains(r#""code":"usage""#));
}
