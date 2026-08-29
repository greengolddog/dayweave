use std::{collections::BTreeSet, sync::Arc};

use axum::{
    Extension, Json, Router,
    extract::{
        Path, Query, State,
        rejection::{JsonRejection, QueryRejection},
    },
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware,
    response::{Html, IntoResponse, Response},
    routing::{delete, get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    AppState,
    auth::Principal,
    config::{GOOGLE_CALENDAR_SCOPE, GOOGLE_TASKS_SCOPE},
    error::{ApiError, ErrorEnvelope},
};

use super::{
    domain::{GoogleAccount, GoogleOAuthCleanupStatus},
    repository::GoogleOAuthRepositoryError,
    service::{
        BeginAuthorization, GoogleOAuthService, GoogleOAuthServiceError, GoogleService,
        OAuthIdempotencyKey,
    },
};

const IDEMPOTENCY_HEADER: &str = "idempotency-key";
const REPLAY_HEADER: &str = "idempotency-replayed";

pub fn protected_routes() -> Router<AppState> {
    Router::new()
        .route("/integrations/google/oauth/start", post(start))
        .route("/integrations/google/accounts", get(accounts))
        .route("/integrations/google/accounts/{id}/pause", post(pause))
        .route("/integrations/google/accounts/{id}/resume", post(resume))
        .route("/integrations/google/accounts/{id}", delete(disconnect))
        .route(
            "/integrations/google/oauth/recovery/acknowledge",
            post(acknowledge_recovery),
        )
        .layer(middleware::map_response(add_no_store))
}

pub fn public_routes() -> Router<AppState> {
    Router::new().route("/v1/integrations/google/oauth/callback", get(callback))
}

#[derive(Clone, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct StartGoogleOAuthRequest {
    #[serde(default)]
    pub services: Vec<GoogleService>,
    #[serde(default)]
    pub force_consent: bool,
    pub login_hint: Option<String>,
    pub account_id: Option<Uuid>,
    #[serde(default)]
    pub connect_new: bool,
    #[serde(default)]
    pub make_default: bool,
}

#[derive(Serialize, ToSchema)]
pub struct StartGoogleOAuthResponse {
    pub authorization_url: String,
    pub expires_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Serialize, ToSchema)]
pub struct GoogleAccountsResponse {
    pub accounts: Vec<GoogleAccount>,
    pub cleanup: GoogleOAuthCleanupStatus,
}

#[derive(Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AccountRevisionRequest {
    pub expected_revision: u64,
}

#[derive(Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AcknowledgeGoogleOAuthRecoveryRequest {
    /// Must be true only after the operator revoked every affected Google
    /// project grant outside `DayWeave`.
    pub project_grants_revoked: bool,
}

#[derive(Serialize, ToSchema)]
pub struct AcknowledgeGoogleOAuthRecoveryResponse {
    pub accounts_marked_reauthorization_required: u64,
    pub legacy_accounts_finalized: u64,
}

#[derive(Deserialize, Serialize)]
pub struct DisconnectQuery {
    pub expected_revision: u64,
}

#[derive(Deserialize)]
pub struct GoogleCallbackQuery {
    state: Option<String>,
    code: Option<String>,
    error: Option<String>,
}

#[utoipa::path(
    post,
    path = "/v1/integrations/google/oauth/start",
    tag = "google",
    security(("bearer_token" = [])),
    params(("Idempotency-Key" = String, Header, description = "8-128 character retry key")),
    request_body = StartGoogleOAuthRequest,
    responses(
        (status = 201, description = "One-time Google authorization URL", body = StartGoogleOAuthResponse),
        (status = 400, description = "Malformed JSON request", body = ErrorEnvelope),
        (status = 422, description = "Missing idempotency key or semantically invalid request", body = ErrorEnvelope),
        (status = 409, description = "Idempotency key conflict or authorization already in progress", body = ErrorEnvelope),
        (status = 401, description = "Missing or invalid token", body = ErrorEnvelope),
        (status = 503, description = "Google OAuth is not configured", body = ErrorEnvelope)
    )
)]
pub(crate) async fn start(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    headers: HeaderMap,
    request: Result<Json<StartGoogleOAuthRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let service = configured_service(&state)?;
    let request = request
        .map_err(|error| ApiError::from_json_rejection(&error))?
        .0;
    if request.services.len() > 2
        || (request.connect_new && request.account_id.is_some())
        || request.login_hint.as_ref().is_some_and(|hint| {
            hint.is_empty() || hint.len() > 320 || hint.chars().any(char::is_control)
        })
    {
        return Err(ApiError::validation("invalid Google OAuth request"));
    }
    let services = request.services.into_iter().collect::<BTreeSet<_>>();
    let idempotency = google_idempotency(
        &headers,
        &principal,
        "google.oauth.start",
        None,
        &json!({
            "services": services.clone(),
            "force_consent": request.force_consent,
            "login_hint": request.login_hint.clone(),
            "account_id": request.account_id,
            "connect_new": request.connect_new,
            "make_default": request.make_default,
        }),
    )?;
    let started = service
        .begin(
            BeginAuthorization {
                owner_subject: principal.subject,
                services,
                force_consent: request.force_consent,
                login_hint: request.login_hint,
                account_id: request.account_id,
                connect_new: request.connect_new,
                make_default: request.make_default,
            },
            idempotency,
        )
        .await
        .map_err(map_service_error)?;
    Ok(json_mutation_response(
        StatusCode::CREATED,
        StartGoogleOAuthResponse {
            authorization_url: started.authorization_url.to_string(),
            expires_at: started.expires_at,
        },
        started.replayed,
    ))
}

#[utoipa::path(
    get,
    path = "/v1/integrations/google/accounts",
    tag = "google",
    security(("bearer_token" = [])),
    responses(
        (status = 200, description = "Connected Google identity without credentials", body = GoogleAccountsResponse),
        (status = 401, description = "Missing or invalid token", body = ErrorEnvelope),
        (status = 503, description = "Google OAuth is not configured", body = ErrorEnvelope)
    )
)]
pub(crate) async fn accounts(State(state): State<AppState>) -> Result<impl IntoResponse, ApiError> {
    let (accounts, cleanup) = configured_service(&state)?
        .accounts_with_cleanup()
        .await
        .map_err(map_service_error)?;
    Ok((
        no_store_headers(),
        Json(GoogleAccountsResponse { accounts, cleanup }),
    ))
}

#[utoipa::path(
    post,
    path = "/v1/integrations/google/accounts/{id}/pause",
    tag = "google",
    security(("bearer_token" = [])),
    params(
        ("id" = Uuid, Path, description = "Google account ID"),
        ("Idempotency-Key" = String, Header, description = "8-128 character retry key")
    ),
    request_body = AccountRevisionRequest,
    responses((status = 200, body = GoogleAccount), (status = 409, body = ErrorEnvelope))
)]
pub(crate) async fn pause(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    request: Result<Json<AccountRevisionRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    set_paused(state, principal, headers, id, request, true).await
}

#[utoipa::path(
    post,
    path = "/v1/integrations/google/accounts/{id}/resume",
    tag = "google",
    security(("bearer_token" = [])),
    params(
        ("id" = Uuid, Path, description = "Google account ID"),
        ("Idempotency-Key" = String, Header, description = "8-128 character retry key")
    ),
    request_body = AccountRevisionRequest,
    responses((status = 200, body = GoogleAccount), (status = 409, body = ErrorEnvelope))
)]
pub(crate) async fn resume(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    request: Result<Json<AccountRevisionRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    set_paused(state, principal, headers, id, request, false).await
}

async fn set_paused(
    state: AppState,
    principal: Principal,
    headers: HeaderMap,
    id: Uuid,
    request: Result<Json<AccountRevisionRequest>, JsonRejection>,
    paused: bool,
) -> Result<Response, ApiError> {
    let request = request
        .map_err(|error| ApiError::from_json_rejection(&error))?
        .0;
    let operation = if paused {
        "google.account.pause"
    } else {
        "google.account.resume"
    };
    let idempotency = google_idempotency(&headers, &principal, operation, Some(id), &request)?;
    let mutation = configured_service(&state)?
        .set_paused(id, request.expected_revision, paused, idempotency)
        .await
        .map_err(map_service_error)?;
    Ok(json_mutation_response(
        StatusCode::OK,
        mutation.account,
        mutation.replayed,
    ))
}

#[utoipa::path(
    delete,
    path = "/v1/integrations/google/accounts/{id}",
    tag = "google",
    security(("bearer_token" = [])),
    params(
        ("id" = Uuid, Path, description = "Google account ID"),
        ("expected_revision" = u64, Query, description = "Optimistic account revision"),
        ("Idempotency-Key" = String, Header, description = "8-128 character retry key")
    ),
    responses(
        (status = 200, description = "Google credential revoked and removed locally", body = GoogleAccount),
        (status = 409, body = ErrorEnvelope),
        (status = 503, description = "Revocation failed; encrypted credentials were retained for retry", body = ErrorEnvelope)
    )
)]
pub(crate) async fn disconnect(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Query(query): Query<DisconnectQuery>,
) -> Result<Response, ApiError> {
    let idempotency = google_idempotency(
        &headers,
        &principal,
        "google.account.disconnect",
        Some(id),
        &query,
    )?;
    let mutation = configured_service(&state)?
        .disconnect(id, query.expected_revision, idempotency)
        .await
        .map_err(map_service_error)?;
    Ok(json_mutation_response(
        StatusCode::OK,
        mutation.account,
        mutation.replayed,
    ))
}

#[utoipa::path(
    post,
    path = "/v1/integrations/google/oauth/recovery/acknowledge",
    tag = "google",
    security(("bearer_token" = [])),
    request_body = AcknowledgeGoogleOAuthRecoveryRequest,
    responses(
        (status = 200, description = "Externally revoked grants were finalized locally", body = AcknowledgeGoogleOAuthRecoveryResponse),
        (status = 400, description = "Malformed JSON request", body = ErrorEnvelope),
        (status = 409, description = "No operator recovery is currently required", body = ErrorEnvelope),
        (status = 422, description = "External project-grant revocation was not confirmed", body = ErrorEnvelope),
        (status = 503, description = "Google OAuth is not configured", body = ErrorEnvelope)
    )
)]
pub(crate) async fn acknowledge_recovery(
    State(state): State<AppState>,
    request: Result<Json<AcknowledgeGoogleOAuthRecoveryRequest>, JsonRejection>,
) -> Result<impl IntoResponse, ApiError> {
    let request = request
        .map_err(|error| ApiError::from_json_rejection(&error))?
        .0;
    let recovered = configured_service(&state)?
        .acknowledge_operator_recovery(request.project_grants_revoked)
        .await
        .map_err(map_service_error)?;
    Ok((
        no_store_headers(),
        Json(AcknowledgeGoogleOAuthRecoveryResponse {
            accounts_marked_reauthorization_required: recovered
                .accounts_marked_reauthorization_required,
            legacy_accounts_finalized: recovered.legacy_accounts_finalized,
        }),
    ))
}

#[utoipa::path(
    get,
    path = "/v1/integrations/google/oauth/callback",
    tag = "google",
    params(
        ("state" = String, Query, description = "One-time OAuth state"),
        ("code" = Option<String>, Query, description = "Google authorization code"),
        ("error" = Option<String>, Query, description = "Google authorization denial")
    ),
    responses(
        (status = 200, description = "Google identity connected"),
        (status = 400, description = "Invalid, denied, expired, or replayed callback"),
        (status = 502, description = "Google token exchange failed"),
        (status = 503, description = "Google OAuth is not configured")
    )
)]
pub(crate) async fn callback(
    State(state): State<AppState>,
    query: Result<Query<GoogleCallbackQuery>, QueryRejection>,
) -> Response {
    let Ok(Query(query)) = query else {
        return callback_page(
            StatusCode::BAD_REQUEST,
            "This Google connection link is malformed.",
        );
    };
    let Some(service) = state.google_oauth.as_ref() else {
        return callback_page(
            StatusCode::SERVICE_UNAVAILABLE,
            "Google connection is not available.",
        );
    };
    let Some(returned_state) = query.state.as_deref() else {
        return callback_page(
            StatusCode::BAD_REQUEST,
            "This Google connection link is invalid.",
        );
    };
    if query.error.is_some() || query.code.is_none() {
        let _ = service.callback_denied(returned_state).await;
        return callback_page(
            StatusCode::BAD_REQUEST,
            "Google connection was cancelled or denied. You may close this window.",
        );
    }
    match service
        .callback(returned_state, query.code.as_deref().unwrap_or_default())
        .await
    {
        Ok(account) => callback_page(StatusCode::OK, callback_success_message(&account)),
        Err(
            GoogleOAuthServiceError::InvalidCallback
            | GoogleOAuthServiceError::Repository(GoogleOAuthRepositoryError::InvalidCallbackState),
        ) => callback_page(
            StatusCode::BAD_REQUEST,
            "This Google connection link is invalid, expired, or was already used.",
        ),
        Err(_) => callback_page(
            StatusCode::BAD_GATEWAY,
            "Google connection could not be completed. Start a new connection from DayWeave.",
        ),
    }
}

fn callback_success_message(account: &GoogleAccount) -> &'static str {
    let calendar = account.granted_scopes.contains(GOOGLE_CALENDAR_SCOPE);
    let tasks = account.granted_scopes.contains(GOOGLE_TASKS_SCOPE);
    match (calendar, tasks) {
        (true, true) => "Google Calendar and Tasks are connected. You may close this window.",
        (true, false) => "Google Calendar is connected. You may close this window.",
        (false, true) => "Google Tasks is connected. You may close this window.",
        (false, false) => "Google connection completed. You may close this window.",
    }
}

fn configured_service(state: &AppState) -> Result<&Arc<GoogleOAuthService>, ApiError> {
    state
        .google_oauth
        .as_ref()
        .ok_or_else(|| ApiError::unavailable("Google OAuth is not configured"))
}

fn google_idempotency<T: Serialize>(
    headers: &HeaderMap,
    principal: &Principal,
    operation: &str,
    account_id: Option<Uuid>,
    request: &T,
) -> Result<OAuthIdempotencyKey, ApiError> {
    let key = headers
        .get(IDEMPOTENCY_HEADER)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| ApiError::validation("Idempotency-Key header is required"))?;
    let mut digest = Sha256::new();
    digest.update(operation.as_bytes());
    digest.update([0]);
    digest.update(principal.subject.as_bytes());
    if let Some(account_id) = account_id {
        digest.update(account_id.as_bytes());
    }
    digest.update(serde_json::to_vec(request).map_err(|_| ApiError::internal())?);
    Ok(OAuthIdempotencyKey {
        key: key.to_owned(),
        fingerprint: digest.finalize().into(),
    })
}

#[allow(clippy::needless_pass_by_value)]
fn map_service_error(error: GoogleOAuthServiceError) -> ApiError {
    match error {
        GoogleOAuthServiceError::InvalidRequest => {
            ApiError::validation("invalid Google OAuth request")
        }
        GoogleOAuthServiceError::OperatorConfirmationRequired => ApiError::validation(
            "confirm external revocation of every affected Google project grant",
        ),
        GoogleOAuthServiceError::InvalidIdempotencyKey => {
            ApiError::validation("Idempotency-Key must be 8-128 URL-safe ASCII characters")
        }
        GoogleOAuthServiceError::InvalidCallback
        | GoogleOAuthServiceError::Repository(GoogleOAuthRepositoryError::InvalidCallbackState) => {
            ApiError::validation("Google OAuth callback is invalid, expired, or already used")
        }
        GoogleOAuthServiceError::Repository(GoogleOAuthRepositoryError::AccountNotFound) => {
            ApiError::not_found("Google account")
        }
        GoogleOAuthServiceError::Repository(GoogleOAuthRepositoryError::RevisionConflict {
            expected,
            actual,
        }) => ApiError::conflict("Google account changed on another device").with_details(json!({
            "expected_revision": expected,
            "actual_revision": actual,
        })),
        GoogleOAuthServiceError::Repository(
            GoogleOAuthRepositoryError::AuthorizationConflict
            | GoogleOAuthRepositoryError::AuthorizationInProgress
            | GoogleOAuthRepositoryError::RevocationInProgress
            | GoogleOAuthRepositoryError::DisconnectInProgress
            | GoogleOAuthRepositoryError::AccountStateConflict
            | GoogleOAuthRepositoryError::IdempotencyConflict
            | GoogleOAuthRepositoryError::IdempotencyInProgress
            | GoogleOAuthRepositoryError::OperatorRecoveryNotRequired,
        ) => ApiError::conflict(error.to_string()),
        GoogleOAuthServiceError::IdentityMismatch => {
            ApiError::conflict("verified Google identity does not match the selected account")
        }
        GoogleOAuthServiceError::MissingRequestedScopes
        | GoogleOAuthServiceError::MissingRefreshToken
        | GoogleOAuthServiceError::InvalidTokenResponse
        | GoogleOAuthServiceError::IntegrationTimeout
        | GoogleOAuthServiceError::CredentialDurabilityPending
        | GoogleOAuthServiceError::Google(_) => ApiError::unavailable(
            "Google authorization failed; encrypted credentials were retained when needed",
        ),
        GoogleOAuthServiceError::Repository(_)
        | GoogleOAuthServiceError::Crypto(_)
        | GoogleOAuthServiceError::CredentialCorrupt => ApiError::internal(),
    }
}

fn no_store_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    headers
}

async fn add_no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
        .headers_mut()
        .insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    response
}

fn json_mutation_response<T: Serialize>(status: StatusCode, value: T, replayed: bool) -> Response {
    let mut response = (status, no_store_headers(), Json(value)).into_response();
    response.headers_mut().insert(
        REPLAY_HEADER,
        HeaderValue::from_static(if replayed { "true" } else { "false" }),
    );
    response
}

fn callback_page(status: StatusCode, message: &'static str) -> Response {
    let body = format!(
        "<!doctype html><html lang=\"en\"><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width\"><title>DayWeave Google connection</title><body><main><h1>DayWeave</h1><p>{message}</p></main></body></html>"
    );
    let mut response = (status, no_store_headers(), Html(body)).into_response();
    response.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'"),
    );
    response.headers_mut().insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    response
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{BTreeMap, BTreeSet},
        sync::{Arc, Mutex},
        time::Duration,
    };

    use async_trait::async_trait;
    use axum::{
        Router,
        body::Body,
        http::{Request, Response},
    };
    use chrono::{DateTime, Utc};
    use dayweave_google::{
        GoogleError,
        oauth::{AuthorizationOptions, OAuthTokenSet},
    };
    use http_body_util::BodyExt;
    use secrecy::{ExposeSecret, SecretString};
    use serde_json::{Value, json};
    use tower::ServiceExt;
    use url::Url;

    use super::*;
    use crate::{
        auth::StaticTokenAuthenticator,
        config::{
            CredentialKey, GOOGLE_CALENDAR_SCOPE, GOOGLE_EMAIL_SCOPE, GOOGLE_OPENID_SCOPE,
            GOOGLE_TASKS_SCOPE,
        },
        google_oauth::{
            AuthorizationMaterial, CallbackClaim, GoogleIdentity, GoogleOAuthRepository,
            GoogleOAuthTransport, InMemoryGoogleOAuthRepository, OAuthScope, SealedSecret,
            SecretCipher, hash_secret,
        },
        http::router,
        proposals::{Clock, InMemoryProposalRepository, ProposalService},
        readiness::Readiness,
    };

    const TOKEN: &str = "google-http-test-token";

    struct FixedClock(DateTime<Utc>);

    impl Clock for FixedClock {
        fn now(&self) -> DateTime<Utc> {
            self.0
        }
    }

    #[derive(Default)]
    struct HttpTransportState {
        exchanges: usize,
        revoked: Vec<String>,
    }

    #[derive(Default)]
    struct HttpTransport(Mutex<HttpTransportState>);

    #[async_trait]
    impl GoogleOAuthTransport for HttpTransport {
        fn begin(
            &self,
            _options: &AuthorizationOptions,
        ) -> Result<AuthorizationMaterial, GoogleError> {
            let state = "http-state-0000000000000000000000000000000000000001";
            Ok(AuthorizationMaterial {
                authorization_url: Url::parse_with_params(
                    "https://accounts.google.test/authorize",
                    [("state", state)],
                )
                .expect("test authorization URL"),
                state: SecretString::from(state),
                verifier: SecretString::from(
                    "http-verifier-0000000000000000000000000000000000000000000000000000000000000001",
                ),
            })
        }

        async fn exchange(
            &self,
            _state: &SecretString,
            _verifier: &SecretString,
            _code: &SecretString,
        ) -> Result<OAuthTokenSet, GoogleError> {
            let mut state = self.0.lock().expect("transport lock");
            state.exchanges += 1;
            Ok(OAuthTokenSet {
                access_token: SecretString::from("http-access-secret"),
                refresh_token: Some(SecretString::from("http-refresh-secret")),
                expires_in_seconds: 3_600,
                token_type: "Bearer".to_owned(),
                granted_scopes: BTreeSet::from([
                    GOOGLE_CALENDAR_SCOPE.to_owned(),
                    GOOGLE_TASKS_SCOPE.to_owned(),
                    GOOGLE_OPENID_SCOPE.to_owned(),
                    GOOGLE_EMAIL_SCOPE.to_owned(),
                ]),
                id_token: None,
            })
        }

        async fn identity(
            &self,
            _access_token: &SecretString,
        ) -> Result<GoogleIdentity, GoogleError> {
            Ok(GoogleIdentity {
                subject: "google-http-user".to_owned(),
                verified_email: Some("owner@example.test".to_owned()),
            })
        }

        async fn revoke(&self, token: &SecretString) -> Result<(), GoogleError> {
            self.0
                .lock()
                .expect("transport lock")
                .revoked
                .push(token.expose_secret().to_owned());
            Ok(())
        }
    }

    fn google_http_app() -> (
        Router,
        Arc<HttpTransport>,
        Arc<InMemoryGoogleOAuthRepository>,
    ) {
        let clock: Arc<dyn Clock> = Arc::new(FixedClock(
            "2026-08-29T10:00:00Z".parse().expect("test time"),
        ));
        let proposals = Arc::new(ProposalService::new(
            Arc::new(InMemoryProposalRepository::default()),
            clock.clone(),
            Duration::from_hours(7 * 24),
        ));
        let transport = Arc::new(HttpTransport::default());
        let repository = Arc::new(InMemoryGoogleOAuthRepository::default());
        let oauth = Arc::new(GoogleOAuthService::new(
            repository.clone(),
            transport.clone(),
            SecretCipher::new(
                Arc::new(BTreeMap::from([(
                    1,
                    CredentialKey::from_test_bytes([7; 32]),
                )])),
                1,
            ),
            OAuthScope {
                workspace_id: Uuid::from_u128(1),
                user_id: Uuid::from_u128(2),
            },
            clock,
            Duration::from_mins(10),
        ));
        let state = AppState::new(
            proposals,
            Arc::new(StaticTokenAuthenticator::from_plaintext(&[TOKEN])),
            Readiness::default(),
        )
        .with_google_oauth(oauth);
        (router(state), transport, repository)
    }

    fn request(
        method: &str,
        uri: &str,
        body: Option<Value>,
        idempotency_key: Option<&str>,
    ) -> Request<Body> {
        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"));
        if let Some(key) = idempotency_key {
            builder = builder.header(IDEMPOTENCY_HEADER, key);
        }
        if body.is_some() {
            builder = builder.header(header::CONTENT_TYPE, "application/json");
        }
        builder
            .body(body.map_or_else(Body::empty, |value| Body::from(value.to_string())))
            .expect("test request")
    }

    async fn response_json(response: Response<Body>) -> Value {
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("response body")
            .to_bytes();
        serde_json::from_slice(&bytes).expect("JSON response")
    }

    #[tokio::test]
    async fn malformed_callback_query_gets_the_full_security_header_policy() {
        let (app, _, _) = google_http_app();
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/v1/integrations/google/oauth/callback?state=one&state=two&code=secret")
                    .body(Body::empty())
                    .expect("callback request"),
            )
            .await
            .expect("callback response");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        assert_eq!(response.headers()[header::PRAGMA], "no-cache");
        assert_eq!(response.headers()[header::REFERRER_POLICY], "no-referrer");
        assert_eq!(
            response.headers()[header::CONTENT_SECURITY_POLICY],
            "default-src 'none'; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'"
        );
        assert!(
            response.headers()[header::CONTENT_TYPE]
                .to_str()
                .expect("content type")
                .starts_with("text/html")
        );
        let body = response
            .into_body()
            .collect()
            .await
            .expect("callback body")
            .to_bytes();
        let body = String::from_utf8(body.to_vec()).expect("callback UTF-8");
        assert!(!body.contains("secret"));
        assert!(!body.contains("one"));
        assert!(!body.contains("two"));
    }

    #[tokio::test]
    async fn recovery_endpoint_requires_confirmation_and_an_active_recovery() {
        let (app, _, _) = google_http_app();
        let unconfirmed = app
            .clone()
            .oneshot(request(
                "POST",
                "/v1/integrations/google/oauth/recovery/acknowledge",
                Some(json!({"project_grants_revoked": false})),
                None,
            ))
            .await
            .expect("recovery response");
        assert_eq!(unconfirmed.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(unconfirmed.headers()[header::CACHE_CONTROL], "no-store");

        let unnecessary = app
            .oneshot(request(
                "POST",
                "/v1/integrations/google/oauth/recovery/acknowledge",
                Some(json!({"project_grants_revoked": true})),
                None,
            ))
            .await
            .expect("recovery response");
        assert_eq!(unnecessary.status(), StatusCode::CONFLICT);
        assert_eq!(unnecessary.headers()[header::CACHE_CONTROL], "no-store");
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn oauth_http_flow_is_non_cacheable_one_use_and_exactly_idempotent() {
        let (app, transport, _) = google_http_app();
        let start_body = json!({"services": ["calendar", "tasks"]});
        let started = app
            .clone()
            .oneshot(request(
                "POST",
                "/v1/integrations/google/oauth/start",
                Some(start_body.clone()),
                Some("http-start-key"),
            ))
            .await
            .expect("start response");
        assert_eq!(started.status(), StatusCode::CREATED);
        assert_eq!(started.headers()[header::CACHE_CONTROL], "no-store");
        assert_eq!(started.headers()[REPLAY_HEADER], "false");
        let started = response_json(started).await;
        let authorization_url = started["authorization_url"]
            .as_str()
            .expect("authorization URL");

        let replayed = app
            .clone()
            .oneshot(request(
                "POST",
                "/v1/integrations/google/oauth/start",
                Some(start_body),
                Some("http-start-key"),
            ))
            .await
            .expect("start replay response");
        assert_eq!(replayed.status(), StatusCode::CREATED);
        assert_eq!(replayed.headers()[REPLAY_HEADER], "true");
        assert_eq!(response_json(replayed).await, started);

        let conflicting_reuse = app
            .clone()
            .oneshot(request(
                "POST",
                "/v1/integrations/google/oauth/start",
                Some(json!({"services": ["calendar"], "force_consent": true})),
                Some("http-start-key"),
            ))
            .await
            .expect("conflicting start response");
        assert_eq!(conflicting_reuse.status(), StatusCode::CONFLICT);
        assert_eq!(
            conflicting_reuse.headers()[header::CACHE_CONTROL],
            "no-store"
        );

        let state = Url::parse(authorization_url)
            .expect("authorization URL parses")
            .query_pairs()
            .find(|(key, _)| key == "state")
            .map(|(_, value)| value.into_owned())
            .expect("state query");
        let callback_uri =
            format!("/v1/integrations/google/oauth/callback?state={state}&code=http-code-secret");
        let callback = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(&callback_uri)
                    .body(Body::empty())
                    .expect("callback request"),
            )
            .await
            .expect("callback response");
        assert_eq!(callback.status(), StatusCode::OK);
        assert_eq!(callback.headers()[header::CACHE_CONTROL], "no-store");
        assert_eq!(callback.headers()[header::REFERRER_POLICY], "no-referrer");
        let callback_body = callback
            .into_body()
            .collect()
            .await
            .expect("callback body")
            .to_bytes();
        let callback_body = String::from_utf8(callback_body.to_vec()).expect("callback UTF-8");
        assert!(!callback_body.contains("http-code-secret"));
        assert!(!callback_body.contains(&state));

        let callback_replay = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(&callback_uri)
                    .body(Body::empty())
                    .expect("callback replay request"),
            )
            .await
            .expect("callback replay response");
        assert_eq!(callback_replay.status(), StatusCode::BAD_REQUEST);
        assert_eq!(transport.0.lock().expect("transport lock").exchanges, 1);

        let accounts = app
            .clone()
            .oneshot(request(
                "GET",
                "/v1/integrations/google/accounts",
                None,
                None,
            ))
            .await
            .expect("accounts response");
        assert_eq!(accounts.status(), StatusCode::OK);
        assert_eq!(accounts.headers()[header::CACHE_CONTROL], "no-store");
        let accounts = response_json(accounts).await;
        assert_eq!(accounts["cleanup"]["held"], 0);
        assert_eq!(accounts["cleanup"]["pending"], 0);
        assert_eq!(accounts["cleanup"]["retrying"], 0);
        let account = &accounts["accounts"][0];
        assert_eq!(account["is_default"], true);
        let account_id = account["id"].as_str().expect("account id");
        let revision = account["revision"].as_u64().expect("account revision");
        assert!(accounts.to_string().find("http-access-secret").is_none());
        assert!(accounts.to_string().find("http-refresh-secret").is_none());

        let pause_uri = format!("/v1/integrations/google/accounts/{account_id}/pause");
        let pause_body = json!({"expected_revision": revision});
        let paused = app
            .clone()
            .oneshot(request(
                "POST",
                &pause_uri,
                Some(pause_body.clone()),
                Some("http-pause-key"),
            ))
            .await
            .expect("pause response");
        assert_eq!(paused.status(), StatusCode::OK);
        assert_eq!(paused.headers()[REPLAY_HEADER], "false");
        let paused = response_json(paused).await;
        assert_eq!(paused["status"], "paused");

        let pause_replay = app
            .clone()
            .oneshot(request(
                "POST",
                &pause_uri,
                Some(pause_body),
                Some("http-pause-key"),
            ))
            .await
            .expect("pause replay response");
        assert_eq!(pause_replay.status(), StatusCode::OK);
        assert_eq!(pause_replay.headers()[REPLAY_HEADER], "true");
        assert_eq!(response_json(pause_replay).await, paused);

        let paused_revision = paused["revision"].as_u64().expect("paused revision");
        let disconnect_uri = format!(
            "/v1/integrations/google/accounts/{account_id}?expected_revision={paused_revision}"
        );
        let disconnected = app
            .clone()
            .oneshot(request(
                "DELETE",
                &disconnect_uri,
                None,
                Some("http-disconnect-key"),
            ))
            .await
            .expect("disconnect response");
        assert_eq!(disconnected.status(), StatusCode::OK);
        assert_eq!(disconnected.headers()[REPLAY_HEADER], "false");
        let disconnected = response_json(disconnected).await;
        assert_eq!(disconnected["status"], "revoked");

        let disconnect_replay = app
            .oneshot(request(
                "DELETE",
                &disconnect_uri,
                None,
                Some("http-disconnect-key"),
            ))
            .await
            .expect("disconnect replay response");
        assert_eq!(disconnect_replay.status(), StatusCode::OK);
        assert_eq!(disconnect_replay.headers()[REPLAY_HEADER], "true");
        assert_eq!(response_json(disconnect_replay).await, disconnected);
        assert_eq!(
            transport.0.lock().expect("transport lock").revoked,
            vec!["http-refresh-secret"]
        );
    }

    #[tokio::test]
    async fn bogus_public_callbacks_do_not_run_global_cleanup() {
        let (app, transport, repository) = google_http_app();
        let started = app
            .clone()
            .oneshot(request(
                "POST",
                "/v1/integrations/google/oauth/start",
                Some(json!({"services": ["calendar", "tasks"]})),
                Some("http-bogus-source"),
            ))
            .await
            .expect("start response");
        let started = response_json(started).await;
        let state = Url::parse(
            started["authorization_url"]
                .as_str()
                .expect("authorization URL"),
        )
        .expect("authorization URL parses")
        .query_pairs()
        .find(|(key, _)| key == "state")
        .map(|(_, value)| value.into_owned())
        .expect("state query");
        let now: DateTime<Utc> = "2026-08-29T10:00:00Z".parse().expect("test time");
        let CallbackClaim::Exchange(claimed) = repository
            .claim_callback(hash_secret(&state), now, now - Duration::from_mins(2))
            .await
            .expect("claim source callback")
        else {
            panic!("new session exchanges");
        };
        repository
            .hold_cleanup_token(
                claimed.id,
                SealedSecret {
                    key_version: 1,
                    ciphertext: vec![7; 64],
                },
                now,
            )
            .await
            .expect("hold cleanup token");
        repository
            .abandon_authorization(claimed.id, now)
            .await
            .expect("promote cleanup token");

        for index in 0..8 {
            let bogus = format!("bogus-http-state-{index:0>32}");
            let callback =
                format!("/v1/integrations/google/oauth/callback?state={bogus}&code=bogus-code");
            let response = app
                .clone()
                .oneshot(request("GET", &callback, None, None))
                .await
                .expect("bogus callback response");
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            let denied =
                format!("/v1/integrations/google/oauth/callback?state={bogus}&error=access_denied");
            let response = app
                .clone()
                .oneshot(request("GET", &denied, None, None))
                .await
                .expect("bogus denial response");
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        }
        assert_eq!(transport.0.lock().expect("transport lock").exchanges, 0);
        assert!(
            transport
                .0
                .lock()
                .expect("transport lock")
                .revoked
                .is_empty()
        );
        assert_eq!(
            repository
                .cleanup_status()
                .await
                .expect("cleanup remains pending")
                .pending,
            1
        );
    }
}
