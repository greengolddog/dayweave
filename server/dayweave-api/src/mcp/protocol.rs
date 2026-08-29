use serde_json::{Map, Value, json};

pub const CURRENT_PROTOCOL_VERSION: &str = "2026-07-28";
pub const LEGACY_PROTOCOL_VERSION: &str = "2025-11-25";
pub const SUPPORTED_PROTOCOL_VERSIONS: [&str; 2] =
    [CURRENT_PROTOCOL_VERSION, LEGACY_PROTOCOL_VERSION];

pub const SAFETY_INSTRUCTIONS: &str = "DayWeave is proposal-only for external assistants. Read only the minimum schedule detail needed for the current request; sensitive items remain redacted. Use simulate_plan before suggesting schedule changes. submit_proposal creates a reviewable Suggestions Inbox entry and never applies, creates, edits, moves, completes, deletes, RSVPs, or publishes canonical data. Clearly distinguish current state, simulated state, and submitted proposals. The user must review and approve every proposal in the DayWeave app.";

#[derive(Clone, Debug)]
pub struct RpcRequest {
    pub id: Option<Value>,
    pub has_id: bool,
    pub method: String,
    pub params: Map<String, Value>,
}

impl RpcRequest {
    pub fn parse(bytes: &[u8]) -> Result<Self, RpcError> {
        let value: Value = serde_json::from_slice(bytes)
            .map_err(|error| RpcError::new(-32700, format!("Parse error: {error}"), None))?;
        let object = value
            .as_object()
            .ok_or_else(|| RpcError::new(-32600, "Invalid Request", None))?;
        if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return Err(RpcError::new(
                -32600,
                "jsonrpc must equal '2.0'",
                object.get("id").cloned(),
            ));
        }
        let method = object
            .get("method")
            .and_then(Value::as_str)
            .filter(|method| !method.is_empty())
            .ok_or_else(|| {
                RpcError::new(
                    -32600,
                    "method must be a non-empty string",
                    object.get("id").cloned(),
                )
            })?
            .to_owned();
        let has_id = object.contains_key("id");
        let id = object.get("id").cloned();
        if id
            .as_ref()
            .is_some_and(|id| !id.is_null() && !id.is_string() && !id.is_number())
        {
            return Err(RpcError::new(
                -32600,
                "id must be a string, number, or null",
                None,
            ));
        }
        let params = match object.get("params") {
            None => Map::new(),
            Some(Value::Object(params)) => params.clone(),
            Some(_) => {
                return Err(RpcError::new(-32602, "params must be an object", id));
            }
        };
        Ok(Self {
            id,
            has_id,
            method,
            params,
        })
    }

    #[must_use]
    pub fn body_protocol_version(&self) -> Option<&str> {
        self.meta()
            .and_then(|meta| meta.get("io.modelcontextprotocol/protocolVersion"))
            .and_then(Value::as_str)
    }

    #[must_use]
    pub fn has_client_capabilities(&self) -> bool {
        self.meta()
            .and_then(|meta| meta.get("io.modelcontextprotocol/clientCapabilities"))
            .is_some_and(Value::is_object)
    }

    #[must_use]
    pub fn client_name(&self) -> Option<String> {
        self.meta()
            .and_then(|meta| meta.get("io.modelcontextprotocol/clientInfo"))
            .and_then(Value::as_object)
            .and_then(|info| info.get("name"))
            .and_then(Value::as_str)
            .map(str::to_owned)
    }

    fn meta(&self) -> Option<&Map<String, Value>> {
        self.params.get("_meta").and_then(Value::as_object)
    }
}

#[derive(Clone, Debug)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    pub data: Option<Value>,
    pub id: Option<Value>,
}

impl RpcError {
    #[must_use]
    pub fn new(code: i64, message: impl Into<String>, id: Option<Value>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
            id,
        }
    }

    #[must_use]
    pub fn with_data(mut self, data: Value) -> Self {
        self.data = Some(data);
        self
    }

    #[must_use]
    pub fn body(&self, request_id: &str) -> Value {
        let mut data = self.data.clone().unwrap_or_else(|| json!({}));
        if let Some(object) = data.as_object_mut() {
            object.insert("requestId".to_owned(), json!(request_id));
        }
        json!({
            "jsonrpc": "2.0",
            "id": self.id,
            "error": {
                "code": self.code,
                "message": self.message,
                "data": data,
            }
        })
    }
}

#[must_use]
pub fn success(id: Option<&Value>, result: &Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    })
}

#[must_use]
pub fn response_meta(request_id: &str) -> Value {
    json!({
        "io.modelcontextprotocol/serverInfo": {
            "name": "dayweave",
            "version": env!("CARGO_PKG_VERSION"),
        },
        "com.greengolddog.dayweave/requestId": request_id,
    })
}

pub fn attach_response_meta(result: &mut Value, request_id: &str) {
    if let Some(object) = result.as_object_mut() {
        let server_meta = response_meta(request_id);
        let metadata = object
            .entry("_meta".to_owned())
            .or_insert_with(|| json!({}));
        if let (Some(metadata), Some(server_meta)) =
            (metadata.as_object_mut(), server_meta.as_object())
        {
            metadata.extend(server_meta.clone());
        }
    }
}

#[must_use]
pub fn discover_result(request_id: &str) -> Value {
    json!({
        "resultType": "complete",
        "supportedVersions": SUPPORTED_PROTOCOL_VERSIONS,
        "capabilities": {
            "tools": { "listChanged": false }
        },
        "_meta": response_meta(request_id),
        "instructions": SAFETY_INSTRUCTIONS,
        "ttlMs": 300_000,
        "cacheScope": "private",
    })
}

#[must_use]
pub fn initialize_result(request_id: &str) -> Value {
    json!({
        "protocolVersion": LEGACY_PROTOCOL_VERSION,
        "capabilities": {
            "tools": { "listChanged": false }
        },
        "serverInfo": {
            "name": "dayweave",
            "version": env!("CARGO_PKG_VERSION"),
        },
        "_meta": response_meta(request_id),
        "instructions": SAFETY_INSTRUCTIONS,
    })
}
