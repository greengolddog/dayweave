use std::io::Write;

use serde::Serialize;
use serde_json::Value;
use zeroize::{Zeroize, Zeroizing};

use crate::{Error, Result};

const MAX_METHOD_BYTES: usize = 256;
const MAX_WIRE_ID_BYTES: usize = 256;

pub(crate) enum Incoming {
    Response {
        id: Value,
        result: std::result::Result<Value, i64>,
    },
    Request {
        id: Value,
        method: String,
        params: Option<Value>,
    },
    Notification {
        method: String,
        params: Option<Value>,
    },
}

#[derive(Serialize)]
struct RequestEnvelope<'a, P> {
    id: u64,
    method: &'a str,
    params: &'a P,
}

#[derive(Serialize)]
struct RequestWithoutParams<'a> {
    id: u64,
    method: &'a str,
}

#[cfg(test)]
#[derive(Serialize)]
struct NotificationEnvelope<'a> {
    method: &'a str,
}

#[derive(Serialize)]
struct SuccessEnvelope<'a, R> {
    id: &'a Value,
    result: R,
}

#[derive(Serialize)]
struct FailureEnvelope<'a> {
    id: &'a Value,
    error: FailureBody,
}

#[derive(Serialize)]
struct FailureBody {
    code: i64,
    message: &'static str,
}

pub(crate) fn encode_request<P: Serialize>(
    id: u64,
    method: &str,
    params: &P,
    max_bytes: usize,
) -> Result<Zeroizing<Vec<u8>>> {
    encode(&RequestEnvelope { id, method, params }, max_bytes)
}

pub(crate) fn encode_request_without_params(
    id: u64,
    method: &str,
    max_bytes: usize,
) -> Result<Zeroizing<Vec<u8>>> {
    encode(&RequestWithoutParams { id, method }, max_bytes)
}

#[cfg(test)]
pub(crate) fn encode_notification(method: &str, max_bytes: usize) -> Result<Zeroizing<Vec<u8>>> {
    encode(&NotificationEnvelope { method }, max_bytes)
}

pub(crate) fn encode_success<R: Serialize>(
    id: &Value,
    result: R,
    max_bytes: usize,
) -> Result<Zeroizing<Vec<u8>>> {
    encode(&SuccessEnvelope { id, result }, max_bytes)
}

pub(crate) fn encode_failure(
    id: &Value,
    code: i64,
    message: &'static str,
    max_bytes: usize,
) -> Result<Zeroizing<Vec<u8>>> {
    encode(
        &FailureEnvelope {
            id,
            error: FailureBody { code, message },
        },
        max_bytes,
    )
}

fn encode<T: Serialize>(value: &T, max_bytes: usize) -> Result<Zeroizing<Vec<u8>>> {
    let body_limit = max_bytes.checked_sub(1).ok_or(Error::RequestTooLarge)?;
    let mut writer = LimitedWriter::new(body_limit);
    if serde_json::to_writer(&mut writer, value).is_err() {
        return if writer.exceeded {
            Err(Error::RequestTooLarge)
        } else {
            Err(Error::InvalidMessage)
        };
    }
    let mut body = writer.into_inner();
    body.push(b'\n');
    Ok(body)
}

struct LimitedWriter {
    body: Vec<u8>,
    max_bytes: usize,
    exceeded: bool,
}

impl LimitedWriter {
    fn new(max_bytes: usize) -> Self {
        Self {
            body: Vec::with_capacity(max_bytes.min(4096)),
            max_bytes,
            exceeded: false,
        }
    }

    fn into_inner(mut self) -> Zeroizing<Vec<u8>> {
        Zeroizing::new(std::mem::take(&mut self.body))
    }
}

impl Drop for LimitedWriter {
    fn drop(&mut self) {
        self.body.zeroize();
    }
}

impl Write for LimitedWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.max_bytes.saturating_sub(self.body.len()) {
            self.exceeded = true;
            return Err(std::io::Error::other("encoded message limit exceeded"));
        }
        self.body.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub(crate) fn decode(line: &[u8]) -> Result<Incoming> {
    let value: Value = serde_json::from_slice(line).map_err(|_| Error::InvalidMessage)?;
    let Value::Object(mut object) = value else {
        return Err(Error::InvalidMessage);
    };

    let id = object.remove("id");
    let method = object.remove("method");
    match (id, method) {
        (Some(id), Some(Value::String(method))) => {
            validate_id(&id)?;
            validate_method(&method)?;
            Ok(Incoming::Request {
                id,
                method,
                params: object.remove("params"),
            })
        }
        (None, Some(Value::String(method))) => {
            validate_method(&method)?;
            Ok(Incoming::Notification {
                method,
                params: object.remove("params"),
            })
        }
        (Some(id), None) => {
            validate_id(&id)?;
            let result = object.remove("result");
            let error = object.remove("error");
            match (result, error) {
                (Some(result), None) => Ok(Incoming::Response {
                    id,
                    result: Ok(result),
                }),
                (None, Some(Value::Object(mut error))) => {
                    let code = error
                        .remove("code")
                        .and_then(|value| value.as_i64())
                        .ok_or(Error::InvalidMessage)?;
                    Ok(Incoming::Response {
                        id,
                        result: Err(code),
                    })
                }
                _ => Err(Error::InvalidMessage),
            }
        }
        _ => Err(Error::InvalidMessage),
    }
}

pub(crate) fn response_id_matches(id: &Value, expected: u64) -> bool {
    id.as_u64() == Some(expected)
}

fn validate_method(method: &str) -> Result<()> {
    if method.is_empty() || method.len() > MAX_METHOD_BYTES {
        return Err(Error::InvalidMessage);
    }
    Ok(())
}

fn validate_id(id: &Value) -> Result<()> {
    let valid = match id {
        Value::String(id) => !id.is_empty() && id.len() <= MAX_WIRE_ID_BYTES,
        Value::Number(number) => number.as_i64().is_some() || number.as_u64().is_some(),
        _ => false,
    };
    if !valid {
        return Err(Error::InvalidMessage);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{Incoming, decode, encode_request, response_id_matches};

    #[test]
    fn decodes_response_and_correlates_only_exact_numeric_id() {
        let message = decode(br#"{"id":7,"result":{"ok":true}}"#).expect("valid response");
        let Incoming::Response { id, result } = message else {
            panic!("expected response");
        };
        assert!(response_id_matches(&id, 7));
        assert!(!response_id_matches(&json!("7"), 7));
        assert_eq!(result.expect("successful response"), json!({"ok": true}));
    }

    #[test]
    fn rejects_ambiguous_response() {
        let result = decode(br#"{"id":7,"result":{},"error":{"code":-1}}"#);
        assert!(result.is_err());
    }

    #[test]
    fn accepts_string_id_for_server_request() {
        let message = decode(
            br#"{"id":"approval-1","method":"item/fileChange/requestApproval","params":{}}"#,
        )
        .expect("valid server request");
        let Incoming::Request { id, method, params } = message else {
            panic!("expected request");
        };
        assert_eq!(id, json!("approval-1"));
        assert_eq!(method, "item/fileChange/requestApproval");
        assert_eq!(params, Some(json!({})));
    }

    #[test]
    fn bounds_encoded_messages() {
        let result = encode_request(1, "test", &json!({"value": "long"}), 8);
        assert!(result.is_err());
    }
}
