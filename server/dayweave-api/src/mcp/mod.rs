mod protocol;
mod tools;

use axum::{
    body::Bytes,
    extract::State,
    http::{
        HeaderMap, HeaderValue, StatusCode,
        header::{ACCEPT, CACHE_CONTROL, CONTENT_TYPE, ORIGIN, PRAGMA, WWW_AUTHENTICATE},
    },
    response::{IntoResponse, Response},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};

pub use tools::{McpRequestContext, McpService};

use crate::{
    AppState,
    auth::{PrincipalAudience, bearer_token_from_headers},
    credential_auth::CredentialKind,
    mcp_oauth::McpOAuthVerifier,
};
use protocol::{
    CURRENT_PROTOCOL_VERSION, LEGACY_PROTOCOL_VERSION, RpcError, RpcRequest,
    SUPPORTED_PROTOCOL_VERSIONS, attach_response_meta, discover_result, initialize_result, success,
};
use tools::{ToolCallError, requires_idempotency_header};

const HEADER_PROTOCOL_VERSION: &str = "mcp-protocol-version";
const HEADER_METHOD: &str = "mcp-method";
const HEADER_NAME: &str = "mcp-name";
const HEADER_IDEMPOTENCY_KEY: &str = "mcp-param-idempotency-key";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProtocolEra {
    Modern,
    Legacy,
}

#[allow(clippy::too_many_lines)]
pub async fn handle_post(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let request_id = request_id(&headers);
    let Some(token) = bearer_token_from_headers(&headers) else {
        return unauthorized(&request_id, state.mcp_oauth.as_deref(), false);
    };
    let principal = if let Some(verifier) = state.mcp_oauth.as_deref() {
        if token.starts_with(CredentialKind::McpClient.prefix()) {
            let Ok(principal) = state.authenticator.authenticate(token).await else {
                return unauthorized(&request_id, None, true);
            };
            principal
        } else {
            let Ok(principal) = verifier.authenticate(token).await else {
                return unauthorized(&request_id, Some(verifier), true);
            };
            principal
        }
    } else {
        let Ok(principal) = state.authenticator.authenticate(token).await else {
            return unauthorized(&request_id, None, true);
        };
        principal
    };
    if let Some(credentials) = state.credential_repository.as_ref()
        && credentials.is_account_deletion_fenced().await != Ok(false)
    {
        return unauthorized(&request_id, state.mcp_oauth.as_deref(), true);
    }
    if !matches!(
        principal.audience,
        PrincipalAudience::Legacy | PrincipalAudience::Mcp | PrincipalAudience::McpOAuth
    ) {
        return unauthorized(
            &request_id,
            (principal.audience == PrincipalAudience::McpOAuth)
                .then_some(state.mcp_oauth.as_deref())
                .flatten(),
            true,
        );
    }
    if let Some(origin) = headers.get(ORIGIN) {
        let allowed = origin.to_str().is_ok_and(|origin| {
            state.mcp.is_origin_allowed(origin)
                && (principal.audience == PrincipalAudience::Legacy
                    || principal
                        .allowed_origins
                        .iter()
                        .any(|allowed| allowed == origin))
        });
        if !allowed {
            return rpc_error_response(
                StatusCode::FORBIDDEN,
                RpcError::new(-33003, "Origin is not allowed", None),
                &request_id,
            );
        }
    }

    if !has_media_type(&headers, CONTENT_TYPE, "application/json") {
        return rpc_error_response(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            RpcError::new(-32600, "Content-Type must be application/json", None),
            &request_id,
        );
    }
    if !has_media_type(&headers, ACCEPT, "application/json")
        || !has_media_type(&headers, ACCEPT, "text/event-stream")
    {
        return rpc_error_response(
            StatusCode::NOT_ACCEPTABLE,
            RpcError::new(
                -32600,
                "Accept must list application/json and text/event-stream",
                None,
            ),
            &request_id,
        );
    }

    let request = match RpcRequest::parse(&body) {
        Ok(request) => request,
        Err(error) => return rpc_error_response(StatusCode::BAD_REQUEST, error, &request_id),
    };

    if request.method == "initialize" {
        return handle_legacy_initialize(&request, &request_id);
    }
    if request.method == "notifications/initialized" {
        return if request.has_id {
            rpc_error_response(
                StatusCode::BAD_REQUEST,
                RpcError::new(
                    -32600,
                    "notifications/initialized must not include an id",
                    request.id,
                ),
                &request_id,
            )
        } else {
            StatusCode::ACCEPTED.into_response()
        };
    }

    let era = match protocol_era(&headers, &request) {
        Ok(era) => era,
        Err(error) => return rpc_error_response(StatusCode::BAD_REQUEST, error, &request_id),
    };
    if era == ProtocolEra::Modern
        && let Err(error) = validate_modern_headers(&headers, &request)
    {
        return rpc_error_response(StatusCode::BAD_REQUEST, error, &request_id);
    }
    if !request.has_id {
        return rpc_error_response(
            StatusCode::BAD_REQUEST,
            RpcError::new(
                -32600,
                "This MCP method must be sent as a request with an id",
                None,
            ),
            &request_id,
        );
    }

    let client_name = request.client_name().or_else(|| {
        headers
            .get("user-agent")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
    });
    let context = McpRequestContext {
        principal,
        request_id: request_id.clone(),
        client_name,
    };

    match request.method.as_str() {
        "server/discover" if era == ProtocolEra::Modern => json_response(
            StatusCode::OK,
            success(request.id.as_ref(), &discover_result(&request_id)),
        ),
        "ping" => {
            let mut result = json!({});
            if era == ProtocolEra::Modern {
                attach_response_meta(&mut result, &request_id);
            }
            json_response(StatusCode::OK, success(request.id.as_ref(), &result))
        }
        "tools/list" => {
            if request
                .params
                .get("cursor")
                .is_some_and(|cursor| !cursor.is_null())
            {
                return rpc_error_response(
                    StatusCode::OK,
                    RpcError::new(
                        -32602,
                        "This tool catalog has no continuation cursor",
                        request.id,
                    ),
                    &request_id,
                );
            }
            let mut result = state
                .mcp
                .tool_catalog(&context.principal, era == ProtocolEra::Modern);
            if era == ProtocolEra::Modern {
                attach_response_meta(&mut result, &request_id);
            }
            json_response(StatusCode::OK, success(request.id.as_ref(), &result))
        }
        "tools/call" => handle_tool_call(&state, &headers, request, era, &context).await,
        _ => rpc_error_response(
            StatusCode::NOT_FOUND,
            RpcError::new(-32601, "Method not found", request.id),
            &request_id,
        ),
    }
}

async fn handle_tool_call(
    state: &AppState,
    headers: &HeaderMap,
    request: RpcRequest,
    era: ProtocolEra,
    context: &McpRequestContext,
) -> Response {
    let Some(name) = request.params.get("name").and_then(Value::as_str) else {
        return rpc_error_response(
            StatusCode::OK,
            RpcError::new(-32602, "tools/call requires params.name", request.id),
            &context.request_id,
        );
    };
    if let Err(error) = state.mcp.authorize_tool(&context.principal, name) {
        return if error.is_unknown_tool() {
            rpc_error_response(
                StatusCode::OK,
                RpcError::new(-32602, error.to_string(), request.id),
                &context.request_id,
            )
        } else {
            tool_error_response(
                request.id.as_ref(),
                error,
                era,
                &context.request_id,
                state.mcp_oauth.as_deref(),
                context.principal.audience,
            )
        };
    }
    let arguments = match request.params.get("arguments") {
        None => json!({}),
        Some(Value::Object(arguments)) => Value::Object(arguments.clone()),
        Some(_) => {
            return rpc_error_response(
                StatusCode::OK,
                RpcError::new(-32602, "tools/call arguments must be an object", request.id),
                &context.request_id,
            );
        }
    };
    if era == ProtocolEra::Modern
        && requires_idempotency_header(name)
        && let Err(error) = validate_idempotency_header(headers, &arguments, request.id.clone())
    {
        return rpc_error_response(StatusCode::BAD_REQUEST, error, &context.request_id);
    }

    match state.mcp.call_tool(context, name, arguments).await {
        Ok(output) => {
            let mut result = json!({
                "content": [{ "type": "text", "text": output.summary }],
                "structuredContent": output.structured,
                "isError": false,
            });
            if era == ProtocolEra::Modern {
                result["resultType"] = json!("complete");
                attach_response_meta(&mut result, &context.request_id);
            }
            json_response(StatusCode::OK, success(request.id.as_ref(), &result))
        }
        Err(error) if error.is_unknown_tool() => rpc_error_response(
            StatusCode::OK,
            RpcError::new(-32602, error.to_string(), request.id),
            &context.request_id,
        ),
        Err(error) => tool_error_response(
            request.id.as_ref(),
            error,
            era,
            &context.request_id,
            state.mcp_oauth.as_deref(),
            context.principal.audience,
        ),
    }
}

#[allow(clippy::needless_pass_by_value)]
fn tool_error_response(
    id: Option<&Value>,
    error: ToolCallError,
    era: ProtocolEra,
    request_id: &str,
    oauth: Option<&McpOAuthVerifier>,
    audience: PrincipalAudience,
) -> Response {
    let mut structured = json!({
        "code": error.code(),
        "message": error.to_string(),
    });
    if let Some(details) = error.details() {
        structured["details"] = details.clone();
    }
    let mut result = json!({
        "content": [{ "type": "text", "text": error.to_string() }],
        "structuredContent": structured,
        "isError": true,
    });
    if audience == PrincipalAudience::McpOAuth
        && let (Some(scope), Some(oauth)) = (error.insufficient_scope(), oauth)
    {
        result["_meta"] = json!({
            "mcp/www_authenticate": [oauth.insufficient_scope_challenge(scope)],
        });
    }
    if era == ProtocolEra::Modern {
        result["resultType"] = json!("complete");
        attach_response_meta(&mut result, request_id);
    }
    json_response(StatusCode::OK, success(id, &result))
}

fn handle_legacy_initialize(request: &RpcRequest, request_id: &str) -> Response {
    if !request.has_id {
        return rpc_error_response(
            StatusCode::BAD_REQUEST,
            RpcError::new(-32600, "initialize requires an id", None),
            request_id,
        );
    }
    if request
        .params
        .get("protocolVersion")
        .and_then(Value::as_str)
        .is_none()
        || !request
            .params
            .get("capabilities")
            .is_some_and(Value::is_object)
        || !request
            .params
            .get("clientInfo")
            .is_some_and(Value::is_object)
    {
        return rpc_error_response(
            StatusCode::BAD_REQUEST,
            RpcError::new(
                -32602,
                "initialize params are incomplete",
                request.id.clone(),
            ),
            request_id,
        );
    }
    json_response(
        StatusCode::OK,
        success(request.id.as_ref(), &initialize_result(request_id)),
    )
}

fn protocol_era(headers: &HeaderMap, request: &RpcRequest) -> Result<ProtocolEra, RpcError> {
    let Some(version) = headers
        .get(HEADER_PROTOCOL_VERSION)
        .and_then(|value| value.to_str().ok())
    else {
        return Err(RpcError::new(
            -32020,
            "Missing MCP-Protocol-Version header",
            request.id.clone(),
        ));
    };
    match version {
        CURRENT_PROTOCOL_VERSION => Ok(ProtocolEra::Modern),
        LEGACY_PROTOCOL_VERSION => Ok(ProtocolEra::Legacy),
        _ => Err(
            RpcError::new(-32022, "Unsupported protocol version", request.id.clone())
                .with_data(json!({ "supportedVersions": SUPPORTED_PROTOCOL_VERSIONS })),
        ),
    }
}

fn validate_modern_headers(headers: &HeaderMap, request: &RpcRequest) -> Result<(), RpcError> {
    let Some(body_version) = request.body_protocol_version() else {
        return Err(RpcError::new(
            -32602,
            "params._meta must include the current protocol version and client capabilities",
            request.id.clone(),
        ));
    };
    if body_version != CURRENT_PROTOCOL_VERSION {
        return Err(RpcError::new(
            -32020,
            "MCP-Protocol-Version header does not match request _meta",
            request.id.clone(),
        ));
    }
    if !request.has_client_capabilities() {
        return Err(RpcError::new(
            -32602,
            "params._meta must include client capabilities",
            request.id.clone(),
        ));
    }
    let method = headers
        .get(HEADER_METHOD)
        .and_then(|value| value.to_str().ok());
    if method != Some(request.method.as_str()) {
        return Err(RpcError::new(
            -32020,
            "Mcp-Method header does not match the JSON-RPC method",
            request.id.clone(),
        ));
    }
    if request.method == "tools/call" {
        let body_name = request.params.get("name").and_then(Value::as_str);
        let header_name = headers
            .get(HEADER_NAME)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| decode_mirrored_header(value).ok());
        if header_name.as_deref() != body_name {
            return Err(RpcError::new(
                -32020,
                "Mcp-Name header does not match params.name",
                request.id.clone(),
            ));
        }
    }
    Ok(())
}

fn validate_idempotency_header(
    headers: &HeaderMap,
    arguments: &Value,
    id: Option<Value>,
) -> Result<(), RpcError> {
    let body_value = arguments.get("idempotency_key").and_then(Value::as_str);
    let header_value = headers
        .get(HEADER_IDEMPOTENCY_KEY)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| decode_mirrored_header(value).ok());
    if body_value.is_none() || header_value.as_deref() != body_value {
        return Err(RpcError::new(
            -32020,
            "Mcp-Param-Idempotency-Key header does not match the tool argument",
            id,
        ));
    }
    Ok(())
}

fn decode_mirrored_header(value: &str) -> Result<String, ()> {
    if let Some(encoded) = value
        .strip_prefix("=?base64?")
        .and_then(|value| value.strip_suffix("?="))
    {
        let bytes = STANDARD.decode(encoded).map_err(|_| ())?;
        String::from_utf8(bytes).map_err(|_| ())
    } else {
        Ok(value.to_owned())
    }
}

fn request_id(headers: &HeaderMap) -> String {
    headers
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("unassigned")
        .to_owned()
}

fn has_media_type(headers: &HeaderMap, name: axum::http::HeaderName, expected: &str) -> bool {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value.split(',').any(|entry| {
                entry
                    .split(';')
                    .next()
                    .is_some_and(|media_type| media_type.trim().eq_ignore_ascii_case(expected))
            })
        })
}

fn unauthorized(
    request_id: &str,
    oauth: Option<&McpOAuthVerifier>,
    invalid_token: bool,
) -> Response {
    let mut response = rpc_error_response(
        StatusCode::UNAUTHORIZED,
        RpcError::new(-33001, "A valid bearer token is required", None),
        request_id,
    );
    let challenge = oauth.map_or_else(
        || {
            "Bearer realm=\"dayweave-native-mcp\", scope=\"schedule:read schedule:simulate suggestions:submit\""
                .to_owned()
        },
        |oauth| {
            oauth.challenge(
                Some("schedule:read schedule:simulate suggestions:submit"),
                invalid_token,
            )
        },
    );
    if let Ok(challenge) = HeaderValue::from_str(&challenge) {
        response.headers_mut().insert(WWW_AUTHENTICATE, challenge);
    }
    response
}

#[allow(clippy::needless_pass_by_value)]
fn rpc_error_response(status: StatusCode, error: RpcError, request_id: &str) -> Response {
    json_response(status, error.body(request_id))
}

#[allow(clippy::needless_pass_by_value)]
fn json_response(status: StatusCode, body: Value) -> Response {
    (
        status,
        [
            (CONTENT_TYPE, HeaderValue::from_static("application/json")),
            (
                CACHE_CONTROL,
                HeaderValue::from_static("no-store, max-age=0"),
            ),
            (PRAGMA, HeaderValue::from_static("no-cache")),
        ],
        body.to_string(),
    )
        .into_response()
}
