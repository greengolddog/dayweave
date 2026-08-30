use std::io::Write as _;
use std::process::{Command, Output, Stdio};

const REQUEST: &[u8] = include_bytes!("fixtures/plan-request-v1.json");
const SUCCESS: &[u8] = include_bytes!("fixtures/plan-success-v1.json");

fn run(args: &[&str], input: &[u8]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_dayweave-scheduler-helper"));
    command
        .args(args)
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn process_emits_one_exact_response_and_no_stderr() {
    let first = run(&[], REQUEST);
    let second = run(&[], REQUEST);
    assert_eq!(first.status.code(), Some(0));
    assert_eq!(first.stdout, SUCCESS);
    assert!(first.stderr.is_empty());
    assert_eq!(first.stdout, second.stdout);
    assert_eq!(first.status.code(), second.status.code());
    assert!(second.stderr.is_empty());
}

#[test]
fn process_rejects_arguments_without_reading_or_logging_them() {
    let output = run(&["private argument"], REQUEST);
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stderr.is_empty());
    assert!(
        !output
            .stdout
            .windows("private argument".len())
            .any(|value| value == b"private argument")
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["result"]["error"]["code"], "invalid_request");
}
