use axum::{
    Extension, Json, Router,
    body::to_bytes,
    extract::{DefaultBodyLimit, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware,
    response::{IntoResponse, Response},
    routing::post,
};
use sha2::{Digest, Sha256};

use crate::{
    AppState,
    auth::{Principal, PrincipalAudience, Scope},
    error::ApiError,
};

use super::{
    AssistantProviderError, AssistantTurnRequest, AssistantTurnResponse, validate_provider_response,
};

const ASSISTANT_BODY_LIMIT_BYTES: usize = 128 * 1024;

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/assistant/turns", post(create_turn))
        .layer(middleware::map_response(add_no_store))
        .layer(DefaultBodyLimit::max(ASSISTANT_BODY_LIMIT_BYTES))
}

#[utoipa::path(
    post,
    path = "/v1/assistant/turns",
    tag = "assistant",
    security(("bearer_token" = [])),
    request_body = AssistantTurnRequest,
    responses(
        (status = 200, description = "Bounded advisory assistant reply with no side effects", body = AssistantTurnResponse),
        (status = 400, description = "Malformed or unknown-field JSON", body = crate::error::ErrorEnvelope),
        (status = 401, description = "Missing, invalid, or non-native credential", body = crate::error::ErrorEnvelope),
        (status = 403, description = "Device credential lacks schedule_read or items_read", body = crate::error::ErrorEnvelope),
        (status = 413, description = "Request body exceeds 128 KiB", body = crate::error::ErrorEnvelope),
        (status = 422, description = "Conversation or redacted context violates a safety bound", body = crate::error::ErrorEnvelope),
        (status = 429, description = "Per-principal, concurrency, or token budget exhausted", body = crate::error::ErrorEnvelope),
        (status = 502, description = "Provider returned an invalid or rejected response", body = crate::error::ErrorEnvelope),
        (status = 503, description = "Assistant is disabled or temporarily unavailable", body = crate::error::ErrorEnvelope)
    )
)]
pub(crate) async fn create_turn(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    request: Request,
) -> Result<Response, ApiError> {
    match principal.audience {
        PrincipalAudience::Legacy => {}
        PrincipalAudience::Device
            if principal.has_scope(Scope::ScheduleRead)
                && principal.has_scope(Scope::ItemsRead) => {}
        PrincipalAudience::Device => return Err(ApiError::forbidden()),
        PrincipalAudience::Mcp | PrincipalAudience::McpOAuth => {
            return Err(ApiError::unauthorized());
        }
    }

    if !has_single_json_content_type(request.headers()) {
        return Err(ApiError::bad_request(
            "Content-Type must be exactly application/json",
        ));
    }
    let body = to_bytes(request.into_body(), ASSISTANT_BODY_LIMIT_BYTES)
        .await
        .map_err(|_| ApiError::payload_too_large("Assistant request body exceeds 128 KiB"))?;
    let value = super::strict_json::parse(&body)
        .map_err(|()| ApiError::bad_request("Assistant request JSON is invalid"))?;
    let request: AssistantTurnRequest = serde_json::from_value(value)
        .map_err(|_| ApiError::bad_request("Assistant request JSON is invalid"))?;
    let principal_key = principal_key(&principal);
    let request = request.validate_and_normalize(principal_key).map_err(|_| {
        ApiError::validation("Assistant turn violates the bounded advisory contract")
    })?;
    let request_id = request.request_id;
    let provider_response = state
        .assistant
        .respond(request)
        .await
        .map_err(map_provider_error)?;
    validate_provider_response(&provider_response).map_err(map_provider_error)?;

    Ok((
        StatusCode::OK,
        Json(AssistantTurnResponse {
            request_id,
            reply: provider_response.reply,
            model: provider_response.model,
            generated_at: provider_response.generated_at,
        }),
    )
        .into_response())
}

fn has_single_json_content_type(headers: &HeaderMap) -> bool {
    let mut values = headers.get_all(header::CONTENT_TYPE).iter();
    let Some(value) = values.next() else {
        return false;
    };
    if values.next().is_some() {
        return false;
    }
    value
        .to_str()
        .ok()
        .and_then(|value| value.parse::<mime::Mime>().ok())
        .is_some_and(|media_type| media_type.essence_str() == "application/json")
}

fn map_provider_error(error: AssistantProviderError) -> ApiError {
    match error {
        AssistantProviderError::Unavailable | AssistantProviderError::TemporarilyUnavailable => {
            ApiError::unavailable("Assistant provider is unavailable")
        }
        AssistantProviderError::Rejected | AssistantProviderError::InvalidResponse => {
            ApiError::bad_gateway("Assistant provider could not produce a valid advisory reply")
        }
        AssistantProviderError::RateLimited => {
            ApiError::rate_limited("Assistant request limit reached; retry manually later")
        }
    }
}

fn principal_key(principal: &Principal) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"dayweave-assistant-principal-v1\0");
    digest.update([match principal.audience {
        PrincipalAudience::Legacy => 0,
        PrincipalAudience::Device => 1,
        PrincipalAudience::Mcp => 2,
        PrincipalAudience::McpOAuth => 3,
    }]);
    if let Some(credential_id) = principal.credential_id {
        digest.update([1]);
        digest.update(credential_id.as_bytes());
    } else {
        digest.update([0]);
        digest.update(principal.subject.len().to_be_bytes());
        digest.update(principal.subject.as_bytes());
    }
    digest.finalize().into()
}

async fn add_no_store(mut response: Response) -> Response {
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-store, max-age=0"),
    );
    response
        .headers_mut()
        .insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    response
}
