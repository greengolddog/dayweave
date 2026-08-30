use axum::{
    Json,
    extract::rejection::JsonRejection,
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Serialize;
use serde_json::Value;
use utoipa::ToSchema;

#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
    details: Option<Value>,
}

impl ApiError {
    #[must_use]
    pub fn unauthorized() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "A valid bearer token is required",
        )
    }

    #[must_use]
    pub fn forbidden() -> Self {
        Self::new(
            StatusCode::FORBIDDEN,
            "forbidden",
            "The authenticated credential is not permitted to perform this operation",
        )
    }

    #[must_use]
    pub fn not_found(resource: &str) -> Self {
        Self::new(
            StatusCode::NOT_FOUND,
            "not_found",
            format!("{resource} was not found"),
        )
    }

    #[must_use]
    pub fn validation(message: impl Into<String>) -> Self {
        Self::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "validation_failed",
            message,
        )
    }

    #[must_use]
    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, "conflict", message)
    }

    #[must_use]
    pub(crate) fn item_execution_active(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, "item_execution_active", message)
    }

    #[must_use]
    pub(crate) fn schedule_publication_stale(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, "schedule_publication_stale", message)
    }

    #[must_use]
    pub(crate) fn execution_schedule_stale(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, "execution_schedule_stale", message)
    }

    #[must_use]
    pub(crate) fn execution_index_exhausted(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, "execution_index_exhausted", message)
    }

    #[must_use]
    pub(crate) fn execution_defer_duration_conflict(message: impl Into<String>) -> Self {
        Self::new(
            StatusCode::CONFLICT,
            "execution_defer_duration_conflict",
            message,
        )
    }

    #[must_use]
    pub(crate) fn schedule_publication_idempotency_conflict(message: impl Into<String>) -> Self {
        Self::new(
            StatusCode::CONFLICT,
            "schedule_publication_idempotency_conflict",
            message,
        )
    }

    #[must_use]
    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "service_unavailable",
            message,
        )
    }

    #[must_use]
    pub fn bad_gateway(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_GATEWAY, "bad_gateway", message)
    }

    #[must_use]
    pub fn internal() -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            "An internal error occurred",
        )
    }

    #[must_use]
    pub fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }

    #[must_use]
    pub fn from_json_rejection(rejection: &JsonRejection) -> Self {
        if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE {
            return Self::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                "payload_too_large",
                "Request body exceeds the route limit",
            );
        }
        Self::new(
            StatusCode::BAD_REQUEST,
            "invalid_json",
            rejection.body_text(),
        )
    }

    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            details: None,
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorEnvelope {
    pub error: ErrorBody,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorBody {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = ErrorEnvelope {
            error: ErrorBody {
                code: self.code.to_owned(),
                message: self.message,
                details: self.details,
            },
        };
        let mut response = (self.status, Json(body)).into_response();
        response.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-store, max-age=0"),
        );
        response
            .headers_mut()
            .insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
        if response.status() == StatusCode::UNAUTHORIZED {
            response.headers_mut().insert(
                header::WWW_AUTHENTICATE,
                HeaderValue::from_static("Bearer realm=\"dayweave\""),
            );
        }
        response
    }
}
