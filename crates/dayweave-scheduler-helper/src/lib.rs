//! Fail-closed, one-shot process boundary for the deterministic scheduler.
//!
//! The helper accepts one bounded JSON request and emits one compact JSON
//! response. It deliberately has no file, network, environment, or clock I/O.

mod limits;
mod shape;
mod strict_json;
mod wire;

use dayweave_core::{PlanRequest, ScheduleError, Scheduler};
use limits::PreflightError;
use serde::{Deserialize, Serialize};
use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind};
use strict_json::StrictJsonError;
use wire::PlanOutput;

pub const MAX_INPUT_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_OUTPUT_BYTES: usize = 16 * 1024 * 1024;
pub const SUCCESS_EXIT_CODE: u8 = 0;
pub const REJECTED_EXIT_CODE: u8 = 2;
pub const INTERNAL_EXIT_CODE: u8 = 70;

const PROTOCOL: &str = "dayweave.scheduler.helper";
const VERSION: u16 = 1;
const OPERATION: &str = "plan";
const INTERNAL_RESPONSE: &[u8] = b"{\"protocol\":\"dayweave.scheduler.helper\",\"version\":1,\"result\":{\"type\":\"error\",\"error\":{\"code\":\"internal_failure\",\"message\":\"The scheduler helper could not complete the request.\"}}}\n";

#[derive(Debug, PartialEq, Eq)]
pub struct ProcessOutput {
    pub stdout: Vec<u8>,
    pub exit_code: u8,
}

#[derive(Debug, Serialize)]
struct ResponseEnvelope {
    protocol: &'static str,
    version: u16,
    result: ResponseResult,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ResponseResult {
    Plan { plan: PlanOutput },
    Error { error: ErrorOutput },
}

#[derive(Debug, Serialize)]
struct ErrorOutput {
    code: ErrorCode,
    message: &'static str,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum ErrorCode {
    RequestTooLarge,
    InvalidUtf8,
    InvalidJson,
    DuplicateJsonKey,
    JsonDepthExceeded,
    UnsupportedProtocol,
    UnsupportedVersion,
    UnsupportedOperation,
    InvalidRequest,
    ResourceLimitExceeded,
    ResponseTooLarge,
    InvalidHorizon,
    InvalidGranularity,
    DuplicateItem,
    InvalidItem,
    InvalidWindow,
    MissingPreviousItem,
    InvalidHierarchy,
    InvalidRecurrence,
    InternalFailure,
}

impl ErrorCode {
    const fn message(self) -> &'static str {
        match self {
            Self::RequestTooLarge => "Request exceeds the supported size limit.",
            Self::InvalidUtf8 => "Request must be valid UTF-8.",
            Self::InvalidJson => "Request must contain one valid JSON value.",
            Self::DuplicateJsonKey => "Request contains a duplicate JSON key.",
            Self::JsonDepthExceeded => "Request exceeds the supported JSON nesting depth.",
            Self::UnsupportedProtocol => "Request uses an unsupported protocol.",
            Self::UnsupportedVersion => "Request uses an unsupported protocol version.",
            Self::UnsupportedOperation => "Request uses an unsupported operation.",
            Self::InvalidRequest => "Request does not match the scheduler contract.",
            Self::ResourceLimitExceeded => "Request exceeds the scheduler work budget.",
            Self::ResponseTooLarge => "Schedule exceeds the supported response size.",
            Self::InvalidHorizon => "Planning horizon is invalid.",
            Self::InvalidGranularity => "Slot granularity is invalid.",
            Self::DuplicateItem => "Request contains a duplicate item identifier.",
            Self::InvalidItem => "Request contains an invalid schedule item.",
            Self::InvalidWindow => "Request contains an invalid time window.",
            Self::MissingPreviousItem => "A previous assignment references an unavailable item.",
            Self::InvalidHierarchy => "Item hierarchy is invalid.",
            Self::InvalidRecurrence => "Recurrence configuration is invalid.",
            Self::InternalFailure => "The scheduler helper could not complete the request.",
        }
    }

    const fn exit_code(self) -> u8 {
        match self {
            Self::InternalFailure => INTERNAL_EXIT_CODE,
            _ => REJECTED_EXIT_CODE,
        }
    }
}

/// Processes one complete stdin payload without performing I/O.
#[must_use]
pub fn process_bytes(input: &[u8]) -> ProcessOutput {
    match process(input) {
        Ok(plan) => encode_plan(plan),
        Err(code) => error_output(code),
    }
}

/// Returns the fail-closed response used when invocation arguments are present.
#[must_use]
pub fn invalid_invocation_output() -> ProcessOutput {
    error_output(ErrorCode::InvalidRequest)
}

/// Returns the fixed response used after a panic or process I/O failure.
#[must_use]
pub fn internal_failure_output() -> ProcessOutput {
    ProcessOutput {
        stdout: INTERNAL_RESPONSE.to_vec(),
        exit_code: INTERNAL_EXIT_CODE,
    }
}

fn process(input: &[u8]) -> Result<PlanOutput, ErrorCode> {
    if input.len() > MAX_INPUT_BYTES {
        return Err(ErrorCode::RequestTooLarge);
    }
    if std::str::from_utf8(input).is_err() {
        return Err(ErrorCode::InvalidUtf8);
    }
    let value = strict_json::parse(input).map_err(|error| match error {
        StrictJsonError::Invalid => ErrorCode::InvalidJson,
        StrictJsonError::DuplicateKey => ErrorCode::DuplicateJsonKey,
        StrictJsonError::DepthExceeded => ErrorCode::JsonDepthExceeded,
        StrictJsonError::ResourceLimit => ErrorCode::ResourceLimitExceeded,
    })?;
    let request_value = decode_envelope(&value)?;
    shape::validate(request_value).map_err(|()| ErrorCode::InvalidRequest)?;
    let request = PlanRequest::deserialize(request_value).map_err(|_| ErrorCode::InvalidRequest)?;
    limits::validate(&request).map_err(map_preflight_error)?;
    let plan = catch_unwind(AssertUnwindSafe(|| Scheduler.plan(&request)))
        .map_err(|_| ErrorCode::InternalFailure)?
        .map_err(|error| map_schedule_error(&error))?;
    PlanOutput::try_from(plan).map_err(|_| ErrorCode::InternalFailure)
}

fn decode_envelope(value: &serde_json::Value) -> Result<&serde_json::Value, ErrorCode> {
    let object = value.as_object().ok_or(ErrorCode::InvalidRequest)?;
    let expected = ["protocol", "version", "operation", "request"];
    if object.len() != expected.len() || expected.iter().any(|field| !object.contains_key(*field)) {
        return Err(ErrorCode::InvalidRequest);
    }
    let protocol = object["protocol"]
        .as_str()
        .ok_or(ErrorCode::InvalidRequest)?;
    if protocol != PROTOCOL {
        return Err(ErrorCode::UnsupportedProtocol);
    }
    let version = object["version"]
        .as_u64()
        .ok_or(ErrorCode::InvalidRequest)?;
    if version != u64::from(VERSION) {
        return Err(ErrorCode::UnsupportedVersion);
    }
    let operation = object["operation"]
        .as_str()
        .ok_or(ErrorCode::InvalidRequest)?;
    if operation != OPERATION {
        return Err(ErrorCode::UnsupportedOperation);
    }
    Ok(&object["request"])
}

fn encode_plan(plan: PlanOutput) -> ProcessOutput {
    let response = ResponseEnvelope {
        protocol: PROTOCOL,
        version: VERSION,
        result: ResponseResult::Plan { plan },
    };
    match encode(&response) {
        Ok(stdout) => ProcessOutput {
            stdout,
            exit_code: SUCCESS_EXIT_CODE,
        },
        Err(EncodeError::TooLarge) => error_output(ErrorCode::ResponseTooLarge),
        Err(EncodeError::Serialization) => internal_failure_output(),
    }
}

fn error_output(code: ErrorCode) -> ProcessOutput {
    let response = ResponseEnvelope {
        protocol: PROTOCOL,
        version: VERSION,
        result: ResponseResult::Error {
            error: ErrorOutput {
                code,
                message: code.message(),
            },
        },
    };
    ProcessOutput {
        stdout: encode(&response).unwrap_or_else(|_| INTERNAL_RESPONSE.to_vec()),
        exit_code: code.exit_code(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EncodeError {
    TooLarge,
    Serialization,
}

struct BoundedWriter {
    bytes: Vec<u8>,
    limit: usize,
    exceeded: bool,
}

impl BoundedWriter {
    const fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
            exceeded: false,
        }
    }
}

impl io::Write for BoundedWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if buffer.len() > self.limit.saturating_sub(self.bytes.len()) {
            self.exceeded = true;
            return Err(io::Error::other("bounded scheduler output exceeded"));
        }
        self.bytes.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn encode(response: &ResponseEnvelope) -> Result<Vec<u8>, EncodeError> {
    let mut output = BoundedWriter::new(MAX_OUTPUT_BYTES - 1);
    if serde_json::to_writer(&mut output, response).is_err() {
        return Err(if output.exceeded {
            EncodeError::TooLarge
        } else {
            EncodeError::Serialization
        });
    }
    output.bytes.push(b'\n');
    Ok(output.bytes)
}

fn map_preflight_error(error: PreflightError) -> ErrorCode {
    match error {
        PreflightError::Schedule(error) => map_schedule_error(&error),
        PreflightError::InvalidRequest => ErrorCode::InvalidRequest,
        PreflightError::ResourceLimit => ErrorCode::ResourceLimitExceeded,
    }
}

const fn map_schedule_error(error: &ScheduleError) -> ErrorCode {
    match error {
        ScheduleError::InvalidHorizon => ErrorCode::InvalidHorizon,
        ScheduleError::InvalidGranularity => ErrorCode::InvalidGranularity,
        ScheduleError::DuplicateItem(_) => ErrorCode::DuplicateItem,
        ScheduleError::InvalidItem { .. } => ErrorCode::InvalidItem,
        ScheduleError::InvalidWindow { .. } => ErrorCode::InvalidWindow,
        ScheduleError::MissingPreviousItem(_) => ErrorCode::MissingPreviousItem,
        ScheduleError::InvalidHierarchy(_) => ErrorCode::InvalidHierarchy,
        ScheduleError::InvalidRecurrence(_) => ErrorCode::InvalidRecurrence,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOLDEN_REQUEST: &[u8] = include_bytes!("../tests/fixtures/plan-request-v1.json");

    fn error_code(output: &ProcessOutput) -> String {
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        value["result"]["error"]["code"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    fn assert_schema_rejected(mutate: impl FnOnce(&mut serde_json::Value)) {
        let mut value: serde_json::Value = serde_json::from_slice(GOLDEN_REQUEST).unwrap();
        mutate(&mut value);
        let output = process_bytes(&serde_json::to_vec(&value).unwrap());
        assert_eq!(error_code(&output), "invalid_request");
        assert!(
            !output
                .stdout
                .windows(15)
                .any(|value| value == b"boundary secret")
        );
    }

    #[test]
    fn golden_request_is_deterministic() {
        let first = process_bytes(GOLDEN_REQUEST);
        let second = process_bytes(GOLDEN_REQUEST);
        assert_eq!(first, second);
        assert_eq!(first.exit_code, SUCCESS_EXIT_CODE);
    }

    #[test]
    fn rejects_invalid_encoding_and_structure_without_echoing_input() {
        let invalid_utf8 = process_bytes(&[0xff]);
        assert_eq!(error_code(&invalid_utf8), "invalid_utf8");

        let duplicate = process_bytes(br#"{"protocol":"x","protocol":"secret"}"#);
        assert_eq!(error_code(&duplicate), "duplicate_json_key");
        assert!(!duplicate.stdout.windows(6).any(|value| value == b"secret"));

        let trailing = process_bytes(br"{}{}");
        assert_eq!(error_code(&trailing), "invalid_json");
        assert_eq!(error_code(&process_bytes(b"")), "invalid_json");
        assert_eq!(
            error_code(&process_bytes(b"\xef\xbb\xbf{}")),
            "invalid_json"
        );
    }

    #[test]
    fn validates_envelope_capabilities_before_the_versioned_request() {
        let protocol =
            process_bytes(br#"{"protocol":"other","version":1,"operation":"plan","request":null}"#);
        assert_eq!(error_code(&protocol), "unsupported_protocol");

        let version = process_bytes(
            br#"{"protocol":"dayweave.scheduler.helper","version":2,"operation":"plan","request":null}"#,
        );
        assert_eq!(error_code(&version), "unsupported_version");

        let operation = process_bytes(
            br#"{"protocol":"dayweave.scheduler.helper","version":1,"operation":"inspect","request":null}"#,
        );
        assert_eq!(error_code(&operation), "unsupported_operation");

        let extra = process_bytes(
            br#"{"protocol":"dayweave.scheduler.helper","version":1,"operation":"plan","request":null,"extra":true}"#,
        );
        assert_eq!(error_code(&extra), "invalid_request");
    }

    #[test]
    fn rejects_unknown_fields_at_nested_core_boundaries() {
        let mut value: serde_json::Value = serde_json::from_slice(GOLDEN_REQUEST).unwrap();
        value["request"]["items"][0]["private_note"] = serde_json::json!("do not echo");
        let encoded = serde_json::to_vec(&value).unwrap();
        let output = process_bytes(&encoded);
        assert_eq!(error_code(&output), "invalid_request");
        assert!(
            !output
                .stdout
                .windows(11)
                .any(|value| value == b"do not echo")
        );
    }

    #[test]
    fn rejects_extra_fields_on_internally_tagged_variants() {
        let mut kind: serde_json::Value = serde_json::from_slice(GOLDEN_REQUEST).unwrap();
        kind["request"]["items"][0]["kind"]["private_note"] = serde_json::json!("kind secret");
        let output = process_bytes(&serde_json::to_vec(&kind).unwrap());
        assert_eq!(error_code(&output), "invalid_request");
        assert!(
            !output
                .stdout
                .windows(11)
                .any(|value| value == b"kind secret")
        );

        let mut split: serde_json::Value = serde_json::from_slice(GOLDEN_REQUEST).unwrap();
        split["request"]["items"][0]["split_policy"]["private_note"] =
            serde_json::json!("split secret");
        let output = process_bytes(&serde_json::to_vec(&split).unwrap());
        assert_eq!(error_code(&output), "invalid_request");
        assert!(
            !output
                .stdout
                .windows(12)
                .any(|value| value == b"split secret")
        );
    }

    #[test]
    fn rejects_unknown_fields_across_every_permissive_input_boundary() {
        assert_schema_rejected(|value| {
            value["request"]["private_note"] = serde_json::json!("boundary secret");
        });
        assert_schema_rejected(|value| {
            value["request"]["items"][0]["duration"]["private_note"] =
                serde_json::json!("boundary secret");
        });
        assert_schema_rejected(|value| {
            value["request"]["items"][0]["priority"]["private_note"] =
                serde_json::json!("boundary secret");
        });
        assert_schema_rejected(|value| {
            value["request"]["availability"][0]["private_note"] =
                serde_json::json!("boundary secret");
        });
        assert_schema_rejected(|value| {
            value["request"]["fixed_blocks"] = serde_json::json!([{
                "id": "00000000-0000-0000-0000-000000000002",
                "is_sensitive": false,
                "title": "Synthetic block",
                "start": "2026-09-01T10:00:00Z",
                "end": "2026-09-01T10:30:00Z",
                "source": "manual",
                "private_note": "boundary secret"
            }]);
        });
        assert_schema_rejected(|value| {
            value["request"]["previous_assignments"] = serde_json::json!([{
                "item_id": "00000000-0000-0000-0000-000000000001",
                "occurrence_id": null,
                "blocks": [{
                    "start": "2026-09-01T08:00:00Z",
                    "end": "2026-09-01T08:30:00Z",
                    "session_index": 0,
                    "private_note": "boundary secret"
                }],
                "pinned": false
            }]);
        });
        assert_schema_rejected(|value| {
            value["request"]["config"]["private_note"] = serde_json::json!("boundary secret");
        });
        assert_schema_rejected(|value| {
            value["request"]["items"][0]["constraints"]["private_note"] =
                serde_json::json!("boundary secret");
        });
        assert_schema_rejected(|value| {
            value["request"]["items"][0]["kind"] = serde_json::json!({
                "type": "recurring_task",
                "recurrence": {
                    "type": "daily",
                    "times_per_day": 1,
                    "private_note": "boundary secret"
                }
            });
        });
        assert_schema_rejected(|value| {
            value["request"]["items"][0]["split_policy"] = serde_json::json!({
                "type": "splittable",
                "minimum_session": 10,
                "maximum_session": 30,
                "maximum_sessions": 3,
                "minimum_gap": 5,
                "maximum_days": null,
                "private_note": "boundary secret"
            });
        });
    }

    #[test]
    fn accepts_omitted_optional_core_fields() {
        let mut basic: serde_json::Value = serde_json::from_slice(GOLDEN_REQUEST).unwrap();
        for field in ["parent_id", "sibling_order", "energy"] {
            basic["request"]["items"][0]
                .as_object_mut()
                .unwrap()
                .remove(field);
        }
        basic["request"]["items"][0]["duration"]
            .as_object_mut()
            .unwrap()
            .remove("remaining");
        basic["request"]["availability"][0]
            .as_object_mut()
            .unwrap()
            .remove("location");
        assert_eq!(
            process_bytes(&serde_json::to_vec(&basic).unwrap()).exit_code,
            SUCCESS_EXIT_CODE
        );

        let optional_kinds = [
            serde_json::json!({
                "type": "habit",
                "recurrence": {"type": "daily", "times_per_day": 1},
                "preserves_streak_when_paused": true
            }),
            serde_json::json!({"type": "routine", "ordered": true}),
            serde_json::json!({"type": "goal", "measures": []}),
            serde_json::json!({
                "type": "calendar_event",
                "start": "2026-09-01T08:00:00Z",
                "end": "2026-09-01T08:30:00Z",
                "immutable": true,
                "all_day": false
            }),
        ];
        for kind in optional_kinds {
            let mut value: serde_json::Value = serde_json::from_slice(GOLDEN_REQUEST).unwrap();
            value["request"]["items"][0]["kind"] = kind;
            assert_eq!(
                process_bytes(&serde_json::to_vec(&value).unwrap()).exit_code,
                SUCCESS_EXIT_CODE
            );
        }

        let mut split: serde_json::Value = serde_json::from_slice(GOLDEN_REQUEST).unwrap();
        split["request"]["items"][0]["split_policy"] = serde_json::json!({
            "type": "splittable",
            "minimum_session": 10,
            "maximum_session": 30,
            "maximum_sessions": 3,
            "minimum_gap": 5
        });
        assert_eq!(
            process_bytes(&serde_json::to_vec(&split).unwrap()).exit_code,
            SUCCESS_EXIT_CODE
        );
    }

    #[test]
    fn schema_error_classification_never_depends_on_input_text() {
        let mut value: serde_json::Value = serde_json::from_slice(GOLDEN_REQUEST).unwrap();
        value["request"]["as_of"] = serde_json::json!("unknown field");
        let output = process_bytes(&serde_json::to_vec(&value).unwrap());
        assert_eq!(error_code(&output), "invalid_request");
    }

    #[test]
    fn rejects_inputs_over_the_byte_and_depth_limits() {
        let oversized = vec![b' '; MAX_INPUT_BYTES + 1];
        assert_eq!(error_code(&process_bytes(&oversized)), "request_too_large");

        let deep = format!("{}null{}", "[".repeat(65), "]".repeat(65));
        assert_eq!(
            error_code(&process_bytes(deep.as_bytes())),
            "json_depth_exceeded"
        );
    }

    #[test]
    fn rejects_excessive_recurrence_before_scheduling() {
        let mut value: serde_json::Value = serde_json::from_slice(GOLDEN_REQUEST).unwrap();
        value["request"]["items"][0]["kind"] = serde_json::json!({"type":"recurring_task","recurrence":{"type":"daily","times_per_day":65535}});
        let encoded = serde_json::to_vec(&value).unwrap();
        assert_eq!(
            error_code(&process_bytes(&encoded)),
            "resource_limit_exceeded"
        );
    }

    #[test]
    fn bounds_hierarchy_depth_before_core_recursion() {
        let mut value: serde_json::Value = serde_json::from_slice(GOLDEN_REQUEST).unwrap();
        let template = value["request"]["items"][0].clone();
        let mut items = Vec::new();
        for index in 1_u16..=257 {
            let mut item = template.clone();
            item["id"] = serde_json::json!(format!("00000000-0000-0000-0000-{index:012x}"));
            item["parent_id"] = if index == 1 {
                serde_json::Value::Null
            } else {
                serde_json::json!(format!("00000000-0000-0000-0000-{:012x}", index - 1))
            };
            items.push(item);
        }
        value["request"]["items"] = serde_json::Value::Array(items);
        let output = process_bytes(&serde_json::to_vec(&value).unwrap());
        assert_eq!(error_code(&output), "resource_limit_exceeded");
    }

    #[test]
    fn rejects_hierarchy_cycles_without_entering_core_recursion() {
        let mut value: serde_json::Value = serde_json::from_slice(GOLDEN_REQUEST).unwrap();
        let first_id = "00000000-0000-0000-0000-000000000001";
        let second_id = "00000000-0000-0000-0000-000000000002";
        let mut first = value["request"]["items"][0].clone();
        first["parent_id"] = serde_json::json!(second_id);
        let mut second = first.clone();
        second["id"] = serde_json::json!(second_id);
        second["parent_id"] = serde_json::json!(first_id);
        value["request"]["items"] = serde_json::json!([first, second]);
        let output = process_bytes(&serde_json::to_vec(&value).unwrap());
        assert_eq!(error_code(&output), "invalid_hierarchy");
    }

    #[test]
    fn budgets_recurrence_previous_assignment_matching() {
        let mut value: serde_json::Value = serde_json::from_slice(GOLDEN_REQUEST).unwrap();
        value["request"]["items"][0]["kind"] = serde_json::json!({
            "type": "recurring_task",
            "recurrence": {"type": "daily", "times_per_day": 100}
        });
        let assignment = serde_json::json!({
            "item_id": "00000000-0000-0000-0000-000000000001",
            "occurrence_id": null,
            "blocks": [{
                "start": "2026-09-01T08:00:00Z",
                "end": "2026-09-01T08:30:00Z",
                "session_index": 0
            }],
            "pinned": false
        });
        value["request"]["previous_assignments"] = serde_json::Value::Array(vec![assignment; 120]);
        let output = process_bytes(&serde_json::to_vec(&value).unwrap());
        assert_eq!(error_code(&output), "resource_limit_exceeded");
    }

    #[test]
    fn budgets_split_shrink_attempts_even_without_availability() {
        let mut value: serde_json::Value = serde_json::from_slice(GOLDEN_REQUEST).unwrap();
        value["request"]["availability"] = serde_json::json!([]);
        value["request"]["config"]["slot_granularity"] = serde_json::json!(1);
        value["request"]["items"][0]["duration"] = serde_json::json!({
            "minimum": 1,
            "expected": 4_294_967_295_u32,
            "maximum": 4_294_967_295_u32,
            "remaining": null,
            "source": "user"
        });
        value["request"]["items"][0]["split_policy"] = serde_json::json!({
            "type": "splittable",
            "minimum_session": 1,
            "maximum_session": 4_294_967_295_u32,
            "maximum_sessions": 1,
            "minimum_gap": 0,
            "maximum_days": null
        });
        let output = process_bytes(&serde_json::to_vec(&value).unwrap());
        assert_eq!(error_code(&output), "resource_limit_exceeded");
    }

    #[test]
    fn rejects_far_rolling_anchors_before_alignment_can_stall() {
        let mut value: serde_json::Value = serde_json::from_slice(GOLDEN_REQUEST).unwrap();
        value["request"]["as_of"] = serde_json::json!("9000-09-01T07:00:00Z");
        value["request"]["horizon_start"] = serde_json::json!("9000-09-01T00:00:00Z");
        value["request"]["horizon_end"] = serde_json::json!("9000-09-02T00:00:00Z");
        value["request"]["availability"][0]["start"] = serde_json::json!("9000-09-01T08:00:00Z");
        value["request"]["availability"][0]["end"] = serde_json::json!("9000-09-01T09:00:00Z");
        value["request"]["items"][0]["created_at"] = serde_json::json!("0001-01-01T00:00:00Z");
        value["request"]["items"][0]["updated_at"] = serde_json::json!("0001-01-01T00:00:00Z");
        value["request"]["items"][0]["kind"] = serde_json::json!({
            "type": "recurring_task",
            "recurrence": {"type": "every_interval", "interval": 1}
        });
        let output = process_bytes(&serde_json::to_vec(&value).unwrap());
        assert_eq!(error_code(&output), "resource_limit_exceeded");
    }

    #[test]
    fn budgets_occurrence_weighted_item_payload_cloning() {
        let mut value: serde_json::Value = serde_json::from_slice(GOLDEN_REQUEST).unwrap();
        value["request"]["availability"] = serde_json::json!([]);
        value["request"]["items"][0]["duration"] = serde_json::Value::Null;
        value["request"]["items"][0]["kind"] = serde_json::json!({
            "type": "recurring_task",
            "recurrence": {"type": "daily", "times_per_day": 1000}
        });
        value["request"]["items"][0]["tags"] = serde_json::Value::Array(
            (0..40)
                .map(|index| serde_json::json!(format!("synthetic-{index}")))
                .collect(),
        );
        let output = process_bytes(&serde_json::to_vec(&value).unwrap());
        assert_eq!(error_code(&output), "resource_limit_exceeded");
    }

    #[test]
    fn budgets_session_weighted_context_messages_before_scheduling() {
        let mut value: serde_json::Value = serde_json::from_slice(GOLDEN_REQUEST).unwrap();
        value["request"]["config"]["slot_granularity"] = serde_json::json!(60);
        value["request"]["availability"] = serde_json::json!([{
            "start": "2026-09-01T08:00:00Z",
            "end": "2026-09-01T08:01:00Z",
            "contexts": [],
            "location": null,
            "energy": "deep"
        }]);
        value["request"]["items"][0]["duration"] = serde_json::json!({
            "minimum": 100,
            "expected": 100,
            "maximum": 100,
            "remaining": null,
            "source": "user"
        });
        value["request"]["items"][0]["split_policy"] = serde_json::json!({
            "type": "splittable",
            "minimum_session": 1,
            "maximum_session": 1,
            "maximum_sessions": 100,
            "minimum_gap": 0,
            "maximum_days": null
        });
        value["request"]["items"][0]["constraints"] = serde_json::json!({
            "required_contexts": [{
                "value": "x".repeat(170_000),
                "strength": {"level": "soft", "weight": 1}
            }]
        });
        let output = process_bytes(&serde_json::to_vec(&value).unwrap());
        assert_eq!(error_code(&output), "resource_limit_exceeded");
    }

    #[test]
    fn budgets_context_allocation_across_long_candidate_searches() {
        let mut value: serde_json::Value = serde_json::from_slice(GOLDEN_REQUEST).unwrap();
        value["request"]["horizon_end"] = serde_json::json!("2026-11-30T00:00:00Z");
        value["request"]["config"]["slot_granularity"] = serde_json::json!(1);
        value["request"]["availability"] = serde_json::json!([{
            "start": "2026-09-01T00:00:00Z",
            "end": "2026-11-30T00:00:00Z",
            "contexts": [],
            "location": null,
            "energy": "deep"
        }]);
        value["request"]["items"][0]["duration"] = serde_json::json!({
            "minimum": 1,
            "expected": 1,
            "maximum": 1,
            "remaining": null,
            "source": "user"
        });
        value["request"]["items"][0]["constraints"] = serde_json::json!({
            "required_contexts": [{
                "value": "x".repeat(1_024),
                "strength": {"level": "soft", "weight": 1}
            }]
        });
        let output = process_bytes(&serde_json::to_vec(&value).unwrap());
        assert_eq!(error_code(&output), "resource_limit_exceeded");
    }

    #[test]
    fn caps_immutable_overlap_violations_before_plan_allocation() {
        let mut value: serde_json::Value = serde_json::from_slice(GOLDEN_REQUEST).unwrap();
        value["request"]["fixed_blocks"] = serde_json::Value::Array(
            (0_u16..142)
                .map(|index| {
                    serde_json::json!({
                        "id": format!("00000000-0000-0000-0001-{index:012x}"),
                        "is_sensitive": false,
                        "title": "Synthetic overlap",
                        "start": "2026-09-01T08:00:00Z",
                        "end": "2026-09-01T08:30:00Z",
                        "source": "manual"
                    })
                })
                .collect(),
        );
        let output = process_bytes(&serde_json::to_vec(&value).unwrap());
        assert_eq!(error_code(&output), "resource_limit_exceeded");
    }

    #[test]
    fn candidate_budget_accounts_for_existing_block_scans() {
        let mut value: serde_json::Value = serde_json::from_slice(GOLDEN_REQUEST).unwrap();
        value["request"]["horizon_end"] = serde_json::json!("2026-11-30T00:00:00Z");
        value["request"]["availability"] = serde_json::json!([{
            "start": "2026-09-01T00:00:00Z",
            "end": "2026-11-30T00:00:00Z",
            "contexts": [],
            "location": null,
            "energy": "deep"
        }]);
        value["request"]["config"]["slot_granularity"] = serde_json::json!(1);
        value["request"]["fixed_blocks"] = serde_json::Value::Array(
            (0_u16..100)
                .map(|index| {
                    serde_json::json!({
                        "id": format!("00000000-0000-0000-0002-{index:012x}"),
                        "is_sensitive": false,
                        "title": "Synthetic fixed block",
                        "start": "2026-09-01T00:00:00Z",
                        "end": "2026-09-01T00:01:00Z",
                        "source": "manual"
                    })
                })
                .collect(),
        );
        let output = process_bytes(&serde_json::to_vec(&value).unwrap());
        assert_eq!(error_code(&output), "resource_limit_exceeded");
    }

    #[test]
    fn scheduler_errors_never_echo_item_content() {
        let mut value: serde_json::Value = serde_json::from_slice(GOLDEN_REQUEST).unwrap();
        value["request"]["items"][0]["title"] = serde_json::json!("private title");
        value["request"]["items"][0]["duration"]["minimum"] = serde_json::json!(0);
        let output = process_bytes(&serde_json::to_vec(&value).unwrap());
        assert_eq!(error_code(&output), "invalid_item");
        assert!(
            !output
                .stdout
                .windows(13)
                .any(|value| value == b"private title")
        );
    }

    #[test]
    fn internal_response_is_fixed_and_bounded() {
        let output = internal_failure_output();
        assert_eq!(output.exit_code, INTERNAL_EXIT_CODE);
        assert_eq!(error_code(&output), "internal_failure");
        assert!(output.stdout.len() <= MAX_OUTPUT_BYTES);
    }

    #[test]
    fn bounded_writer_refuses_to_allocate_past_its_limit() {
        let mut writer = BoundedWriter::new(4);
        std::io::Write::write_all(&mut writer, b"1234").unwrap();
        assert!(std::io::Write::write_all(&mut writer, b"5").is_err());
        assert_eq!(writer.bytes, b"1234");
        assert!(writer.exceeded);
    }
}
