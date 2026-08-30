use std::io::{ErrorKind, Write as _};
use std::process::{Command, Output, Stdio};

const REQUEST: &[u8] = include_bytes!("fixtures/plan-request-v1.json");
const SUCCESS: &[u8] = include_bytes!("fixtures/plan-success-v1.json");
const COMPOSE_REQUEST: &[u8] = include_bytes!("fixtures/compose-request-v1.json");
const COMPOSE_SUCCESS: &[u8] = include_bytes!("fixtures/compose-success-v1.json");

fn run(args: &[&str], input: &[u8]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_dayweave-scheduler-helper"));
    command
        .args(args)
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().unwrap();
    if let Err(error) = child.stdin.take().unwrap().write_all(input) {
        // Invalid invocations deliberately exit before reading stdin. Whether
        // the parent finishes this tiny write first is scheduler-dependent.
        assert_eq!(error.kind(), ErrorKind::BrokenPipe);
    }
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
fn compose_process_emits_one_exact_response_and_no_stderr() {
    let first = run(&[], COMPOSE_REQUEST);
    let second = run(&[], COMPOSE_REQUEST);
    assert_eq!(first.status.code(), Some(0));
    assert_eq!(first.stdout, COMPOSE_SUCCESS);
    assert!(first.stderr.is_empty());
    assert_eq!(first.stdout, second.stdout);
    assert_eq!(first.status.code(), second.status.code());
    assert!(second.stderr.is_empty());
}

#[test]
fn compose_and_direct_plan_golden_outputs_share_the_exact_plan() {
    let plan: serde_json::Value = serde_json::from_slice(SUCCESS).unwrap();
    let composition: serde_json::Value = serde_json::from_slice(COMPOSE_SUCCESS).unwrap();
    assert_eq!(
        plan["result"]["plan"],
        composition["result"]["composition"]["plan"]
    );
}

#[test]
fn compose_process_rejection_does_not_echo_sensitive_input() {
    let mut request: serde_json::Value = serde_json::from_slice(COMPOSE_REQUEST).unwrap();
    request["request"]["canonical_items"][0]["title"] =
        serde_json::json!(" boundary process secret ");
    let output = run(&[], &serde_json::to_vec(&request).unwrap());
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stderr.is_empty());
    assert!(
        !output
            .stdout
            .windows("boundary process secret".len())
            .any(|value| value == b"boundary process secret")
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["result"]["error"]["code"], "invalid_item");
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
