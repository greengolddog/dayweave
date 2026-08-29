use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

#[cfg(test)]
use std::process::Stdio;

use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
#[cfg(test)]
use tokio::process::Command;
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{ChildStdin, ChildStdout},
    time,
};
use zeroize::{Zeroize, Zeroizing};

#[cfg(test)]
use crate::protocol::encode_notification;
use crate::{
    Account, AccountState, BedrockCredentialSource, BrowserLogin, CodexAppServerConfig,
    DeviceCodeLogin, Error, ProtocolLimits, Result, ServerInfo, StructuredTurn,
    StructuredTurnRequest, ThreadHandle, ThreadOptions,
    config::canonical_directory,
    error::transport_error,
    process::{CancellationGuard, ManagedChild},
    protocol::{
        Incoming, decode, encode_failure, encode_request, encode_request_without_params,
        encode_success, response_id_matches,
    },
};

const MAX_IDENTIFIER_BYTES: usize = 256;
const MAX_SHORT_STRING_BYTES: usize = 512;
const MAX_URL_BYTES: usize = 8 * 1024;
const METHOD_NOT_FOUND: i64 = -32_601;

pub struct CodexAppServer {
    child: ManagedChild,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    limits: ProtocolLimits,
    #[cfg(test)]
    codex_home: PathBuf,
    next_id: u64,
    unusable: bool,
    server_info: Option<ServerInfo>,
    login_notifications: VecDeque<LoginNotification>,
    turn_notifications: VecDeque<TurnNotification>,
    queued_bytes: usize,
    retained_output_bytes: usize,
    active_turn: Option<ActiveTurn>,
    workspace_roots: Vec<PathBuf>,
    poisoned: Arc<AtomicBool>,
    #[cfg(test)]
    _owned_home: crate::config::OwnedCodexHome,
}

impl CodexAppServer {
    /// Validates the secure scaffold and refuses process startup.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NoSupportedRuntime`] for a valid configuration. No
    /// executable is run. Invalid filesystem/allowlist configuration is
    /// rejected first.
    #[allow(clippy::unused_async)]
    pub async fn spawn(config: CodexAppServerConfig) -> Result<Self> {
        config.validate_scaffold()?;
        // A Unix process group cannot stop an adversarial descendant from
        // calling setsid(2). DayWeave has not established a content-pinned
        // Codex build plus a macOS containment primitive that closes that gap,
        // so executing even a schema-compatible candidate is forbidden.
        Err(Error::NoSupportedRuntime)
    }

    #[cfg(test)]
    async fn spawn_test_runtime(config: &CodexAppServerConfig) -> Result<Self> {
        let prepared = config.prepare_test_runtime()?;
        let codex_home = prepared.codex_home.as_path().to_owned();
        let mut command = Command::new(&prepared.executable);
        command
            .arg("app-server")
            .arg("--stdio")
            .env_clear()
            .env("CODEX_HOME", &codex_home)
            .current_dir(&codex_home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        #[cfg(unix)]
        command.process_group(0);
        for (key, value) in prepared.environment.iter() {
            command.env(key, value);
        }

        let mut spawned_child = command
            .spawn()
            .map_err(|error| Error::Spawn { kind: error.kind() })?;
        drop(command);
        drop(prepared.environment);
        let stdin = spawned_child.stdin.take().ok_or(Error::Spawn {
            kind: std::io::ErrorKind::BrokenPipe,
        })?;
        let stdout = spawned_child.stdout.take().ok_or(Error::Spawn {
            kind: std::io::ErrorKind::BrokenPipe,
        })?;
        let child = ManagedChild::from_spawned(spawned_child)?;

        let mut server = Self {
            child,
            stdin: Some(stdin),
            stdout: BufReader::new(stdout),
            limits: prepared.limits,
            codex_home,
            next_id: 1,
            unusable: false,
            server_info: None,
            login_notifications: VecDeque::new(),
            turn_notifications: VecDeque::new(),
            queued_bytes: 0,
            retained_output_bytes: 0,
            active_turn: None,
            workspace_roots: prepared.workspace_roots,
            poisoned: Arc::new(AtomicBool::new(false)),
            _owned_home: prepared.codex_home,
        };

        match server.initialize().await {
            Ok(info) => {
                server.server_info = Some(info);
                Ok(server)
            }
            Err(error) => {
                server.abort_process().await;
                Err(error)
            }
        }
    }

    #[must_use]
    /// Returns metadata negotiated during initialization.
    ///
    /// # Panics
    ///
    /// This cannot panic for an instance returned by [`Self::spawn`].
    pub fn server_info(&self) -> &ServerInfo {
        self.server_info
            .as_ref()
            .expect("a public CodexAppServer is initialized")
    }

    /// Reads the current authentication state without refreshing credentials.
    ///
    /// # Errors
    ///
    /// Returns a transport, protocol, timeout, or server RPC error.
    pub async fn read_account(&mut self) -> Result<AccountState> {
        let guard = self.operation_guard()?;
        let result = async {
            let response: AccountReadResponse = self
                .request(
                    "account/read",
                    &AccountReadParams {
                        refresh_token: false,
                    },
                )
                .await?;
            let account = response.account.map(AccountWire::into_public).transpose()?;
            Ok(AccountState {
                account,
                requires_openai_auth: response.requires_openai_auth,
            })
        }
        .await;
        self.settle_operation(result, guard).await
    }

    /// Starts API-key authentication without retaining or exposing the key.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty key or a failed App Server request.
    pub async fn login_with_api_key(&mut self, api_key: &SecretString) -> Result<()> {
        if api_key.expose_secret().is_empty() {
            return Err(Error::InvalidConfiguration("API key must be non-empty"));
        }
        let guard = self.operation_guard()?;
        let result = async {
            let response: LoginResponse = self
                .request(
                    "account/login/start",
                    &ApiKeyLoginParams {
                        kind: "apiKey",
                        api_key: api_key.expose_secret(),
                    },
                )
                .await?;
            if response.kind != "apiKey" {
                return Err(Error::InvalidMessage);
            }
            Ok(())
        }
        .await;
        self.settle_operation(result, guard).await
    }

    /// Starts the browser-based `ChatGPT` login flow.
    ///
    /// # Errors
    ///
    /// Returns a transport, protocol, timeout, or server RPC error.
    pub async fn start_chatgpt_login(&mut self) -> Result<BrowserLogin> {
        let guard = self.operation_guard()?;
        let result = async {
            let response: LoginResponse = self
                .request(
                    "account/login/start",
                    &ChatGptLoginParams {
                        kind: "chatgpt",
                        use_hosted_login_success_page: true,
                        app_brand: "chatgpt",
                    },
                )
                .await?;
            let LoginResponse {
                kind,
                login_id,
                auth_url,
                verification_url: _,
                user_code: _,
            } = response;
            if kind != "chatgpt" {
                return Err(Error::InvalidMessage);
            }
            let login_id = required_secret_identifier(login_id)?;
            let auth_url = required_secret_bounded(auth_url, MAX_URL_BYTES)?;
            Ok(BrowserLogin { login_id, auth_url })
        }
        .await;
        self.settle_operation(result, guard).await
    }

    /// Starts the `ChatGPT` device-code login flow.
    ///
    /// # Errors
    ///
    /// Returns a transport, protocol, timeout, or server RPC error.
    pub async fn start_device_code_login(&mut self) -> Result<DeviceCodeLogin> {
        let guard = self.operation_guard()?;
        let result = async {
            let response: LoginResponse = self
                .request(
                    "account/login/start",
                    &DeviceCodeLoginParams {
                        kind: "chatgptDeviceCode",
                    },
                )
                .await?;
            let LoginResponse {
                kind,
                login_id,
                auth_url: _,
                verification_url,
                user_code,
            } = response;
            if kind != "chatgptDeviceCode" {
                return Err(Error::InvalidMessage);
            }
            Ok(DeviceCodeLogin {
                login_id: required_secret_identifier(login_id)?,
                verification_url: required_secret_bounded(verification_url, MAX_URL_BYTES)?,
                user_code: required_secret_short(user_code)?,
            })
        }
        .await;
        self.settle_operation(result, guard).await
    }

    /// Waits for the matching `account/login/completed` notification.
    ///
    /// A timeout invalidates the connection because cancelling a partial JSONL
    /// read cannot safely preserve message framing.
    ///
    /// # Errors
    ///
    /// Returns an error if the matching login fails, the wait times out, or the
    /// protocol connection fails.
    pub async fn wait_for_login(&mut self, login_id: &str, timeout: Duration) -> Result<()> {
        validate_identifier(login_id)?;
        if timeout.is_zero() {
            return Err(Error::InvalidConfiguration("timeout must be non-zero"));
        }
        let guard = self.operation_guard()?;
        let result = async {
            if let Some(result) = self.take_login_notification(login_id) {
                return login_result(&result);
            }
            time::timeout(timeout, self.wait_for_login_inner(login_id))
                .await
                .unwrap_or(Err(Error::Timeout))
        }
        .await;
        self.settle_operation(result, guard).await
    }

    /// Logs out the active App Server account.
    ///
    /// # Errors
    ///
    /// Returns a transport, protocol, timeout, or server RPC error.
    pub async fn logout(&mut self) -> Result<()> {
        let guard = self.operation_guard()?;
        let result = async {
            let _: EmptyResponse = self.request_without_params("account/logout").await?;
            Ok(())
        }
        .await;
        self.settle_operation(result, guard).await
    }

    /// Starts a read-only thread rooted at the explicitly supplied directory.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid options or a failed App Server request.
    pub async fn start_thread(&mut self, options: &ThreadOptions) -> Result<ThreadHandle> {
        let cwd = prepare_thread_options(options, &self.workspace_roots)?;
        let guard = self.operation_guard()?;
        let result = async {
            let params = ThreadStartParams {
                cwd: path_as_str(&cwd)?,
                model: options.model.as_deref(),
                approval_policy: "never",
                sandbox: "read-only",
                service_name: "dayweave",
            };
            let response: ThreadResponse = self.request("thread/start", &params).await?;
            let id = checked_identifier(response.thread.id)?;
            Ok(ThreadHandle { id, cwd })
        }
        .await;
        self.settle_operation(result, guard).await
    }

    /// Resumes a thread with read-only, deny-all policy overrides.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid options or a failed App Server request.
    pub async fn resume_thread(
        &mut self,
        thread: &ThreadHandle,
        options: &ThreadOptions,
    ) -> Result<ThreadHandle> {
        validate_identifier(&thread.id)?;
        let cwd = prepare_thread_options(options, &self.workspace_roots)?;
        let guard = self.operation_guard()?;
        let result = async {
            let params = ThreadResumeParams {
                thread_id: &thread.id,
                cwd: path_as_str(&cwd)?,
                model: options.model.as_deref(),
                approval_policy: "never",
                sandbox: "read-only",
            };
            let response: ThreadResponse = self.request("thread/resume", &params).await?;
            let response_id = checked_identifier(response.thread.id)?;
            if response_id != thread.id {
                return Err(Error::InvalidMessage);
            }
            Ok(ThreadHandle {
                id: response_id,
                cwd,
            })
        }
        .await;
        self.settle_operation(result, guard).await
    }

    /// Runs one turn constrained by the supplied JSON output schema.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid input, transport/protocol failure, a
    /// non-completed turn, or output that cannot deserialize into `T`.
    pub async fn run_structured_turn<T: DeserializeOwned>(
        &mut self,
        thread: &ThreadHandle,
        request: &StructuredTurnRequest,
    ) -> Result<StructuredTurn<T>> {
        validate_identifier(&thread.id)?;
        if request.prompt.expose_secret().len() > self.limits.max_prompt_bytes {
            return Err(Error::PromptTooLarge);
        }
        if !request.output_schema.is_object() {
            return Err(Error::InvalidConfiguration(
                "output schema must be a JSON object",
            ));
        }
        validate_optional_short(request.model.as_deref(), "model is invalid")?;
        validate_optional_short(request.effort.as_deref(), "effort is invalid")?;

        let guard = self.operation_guard()?;
        let timeout = self.limits.turn_timeout;
        let result = time::timeout(timeout, self.run_structured_turn_inner(thread, request))
            .await
            .unwrap_or(Err(Error::Timeout));
        self.settle_operation(result, guard).await
    }

    /// Closes stdin and waits for the subprocess to terminate.
    ///
    /// # Errors
    ///
    /// Returns an error if waiting fails or the subprocess exits unsuccessfully.
    pub async fn shutdown(mut self) -> Result<()> {
        self.stdin.take();
        if self.child.teardown_arranged_or_complete() {
            return Err(Error::ConnectionUnusable);
        }
        // Give a cooperative server a brief chance to observe EOF. We do not
        // reap here: `terminate_and_reap` must signal the originally verified
        // process group before collecting the leader's status.
        time::sleep(self.limits.shutdown_timeout.min(Duration::from_millis(20))).await;
        let status = self
            .child
            .terminate_and_reap(self.limits.shutdown_timeout)
            .await?;
        if status.success() {
            Ok(())
        } else {
            Err(Error::ProcessExited {
                code: status.code(),
            })
        }
    }

    #[cfg(test)]
    async fn initialize(&mut self) -> Result<ServerInfo> {
        let response: InitializeResponse = self
            .request(
                "initialize",
                &InitializeParams {
                    client_info: ClientInfo {
                        name: "dayweave",
                        title: "DayWeave",
                        version: env!("CARGO_PKG_VERSION"),
                    },
                },
            )
            .await?;
        let reported_home = canonical_directory(
            &response.codex_home,
            "App Server returned an invalid CODEX_HOME",
        )?;
        if reported_home != self.codex_home {
            return Err(Error::CodexHomeMismatch);
        }
        let platform_family = checked_short(response.platform_family)?;
        let platform_os = checked_short(response.platform_os)?;
        checked_short(response.user_agent)?;
        self.notification("initialized").await?;
        Ok(ServerInfo {
            platform_family,
            platform_os,
        })
    }

    async fn wait_for_login_inner(&mut self, login_id: &str) -> Result<()> {
        loop {
            match self.read_incoming().await? {
                Incoming::Request { id, method, params } => {
                    if self.deny_request(&id, &method, params).await?.is_fatal() {
                        return self.interrupt_and_fail(Error::InvalidMessage).await;
                    }
                }
                Incoming::Notification { method, params } => {
                    self.accept_notification(&method, params)?;
                    if let Some(result) = self.take_login_notification(login_id) {
                        return login_result(&result);
                    }
                }
                Incoming::Response { .. } => {
                    self.mark_unusable();
                    return Err(Error::UnexpectedResponseId);
                }
            }
        }
    }

    async fn run_structured_turn_inner<T: DeserializeOwned>(
        &mut self,
        thread: &ThreadHandle,
        request: &StructuredTurnRequest,
    ) -> Result<StructuredTurn<T>> {
        let current_cwd = canonical_directory(&thread.cwd, "thread cwd is not a directory")?;
        if current_cwd != thread.cwd || !cwd_is_allowed(&current_cwd, &self.workspace_roots) {
            return self.fail_connection(Error::InvalidMessage).await;
        }
        let input = [TextInput {
            kind: "text",
            text: request.prompt.expose_secret(),
        }];
        let params = TurnStartParams {
            thread_id: &thread.id,
            input: &input,
            output_schema: &request.output_schema,
            approval_policy: "never",
            sandbox_policy: ReadOnlySandbox {
                kind: "readOnly",
                network_access: false,
                access: RestrictedReadAccess {
                    kind: "restricted",
                    include_platform_defaults: false,
                    readable_roots: [path_as_str(&thread.cwd)?],
                },
            },
            model: request.model.as_deref(),
            effort: request.effort.as_deref(),
        };
        let response: TurnStartResponse = self.request("turn/start", &params).await?;
        let turn_id = match checked_identifier(response.turn.id) {
            Ok(turn_id) => turn_id,
            Err(error) => return self.fail_connection(error).await,
        };
        if response.turn.status != "inProgress" {
            return self.fail_connection(Error::InvalidMessage).await;
        }
        self.active_turn = Some(ActiveTurn {
            thread_id: thread.id.clone(),
            turn_id: turn_id.clone(),
        });

        let mut latest_agent_text = None;
        loop {
            while let Some(notification) = self.turn_notifications.pop_front() {
                self.queued_bytes = self.queued_bytes.saturating_sub(notification.byte_len());
                if let Some(completion) = apply_turn_notification(
                    notification,
                    &thread.id,
                    &turn_id,
                    &mut latest_agent_text,
                ) {
                    self.active_turn = None;
                    self.retained_output_bytes = 0;
                    return finish_turn(turn_id, completion, latest_agent_text);
                }
                self.retained_output_bytes =
                    latest_agent_text.as_ref().map_or(0, |text| text.len());
            }

            match self.read_incoming().await? {
                Incoming::Request { id, method, params } => {
                    if self.deny_request(&id, &method, params).await?.is_fatal() {
                        return self.interrupt_and_fail(Error::InvalidMessage).await;
                    }
                }
                Incoming::Notification { method, params } => {
                    self.accept_notification(&method, params)?;
                }
                Incoming::Response { .. } => {
                    self.mark_unusable();
                    return Err(Error::UnexpectedResponseId);
                }
            }
        }
    }

    async fn request<P: Serialize, R: DeserializeOwned>(
        &mut self,
        method: &'static str,
        params: &P,
    ) -> Result<R> {
        self.ensure_usable()?;
        let id = self.allocate_id()?;
        let body = encode_request(id, method, params, self.limits.max_request_bytes)?;
        self.exchange(id, body).await
    }

    async fn request_without_params<R: DeserializeOwned>(
        &mut self,
        method: &'static str,
    ) -> Result<R> {
        self.ensure_usable()?;
        let id = self.allocate_id()?;
        let body = encode_request_without_params(id, method, self.limits.max_request_bytes)?;
        self.exchange(id, body).await
    }

    async fn exchange<R: DeserializeOwned>(
        &mut self,
        id: u64,
        body: Zeroizing<Vec<u8>>,
    ) -> Result<R> {
        let result =
            time::timeout(self.limits.request_timeout, self.exchange_inner(id, &body)).await;
        if let Ok(result) = result {
            result
        } else {
            self.mark_unusable();
            self.abort_process().await;
            Err(Error::Timeout)
        }
    }

    async fn exchange_inner<R: DeserializeOwned>(&mut self, id: u64, body: &[u8]) -> Result<R> {
        self.write_body(body).await?;
        loop {
            match self.read_incoming().await? {
                Incoming::Response {
                    id: response_id,
                    result,
                } => {
                    if !response_id_matches(&response_id, id) {
                        self.mark_unusable();
                        return Err(Error::UnexpectedResponseId);
                    }
                    let value = result.map_err(|code| Error::Rpc { code })?;
                    return if let Ok(response) = serde_json::from_value(value) {
                        Ok(response)
                    } else {
                        self.mark_unusable();
                        Err(Error::InvalidMessage)
                    };
                }
                Incoming::Request { id, method, params } => {
                    if self.deny_request(&id, &method, params).await?.is_fatal() {
                        return self.interrupt_and_fail(Error::InvalidMessage).await;
                    }
                }
                Incoming::Notification { method, params } => {
                    self.accept_notification(&method, params)?;
                }
            }
        }
    }

    #[cfg(test)]
    async fn notification(&mut self, method: &'static str) -> Result<()> {
        self.ensure_usable()?;
        let body = encode_notification(method, self.limits.max_request_bytes)?;
        let result = time::timeout(self.limits.request_timeout, self.write_body(&body)).await;
        if let Ok(result) = result {
            result
        } else {
            self.mark_unusable();
            self.abort_process().await;
            Err(Error::Timeout)
        }
    }

    async fn read_incoming(&mut self) -> Result<Incoming> {
        let mut line = Zeroizing::new(Vec::with_capacity(self.limits.max_line_bytes.min(4096)));
        let read_limit = self
            .limits
            .max_line_bytes
            .checked_add(1)
            .ok_or(Error::InvalidConfiguration("line-size limit overflow"))?;
        let mut limited = (&mut self.stdout).take(read_limit as u64);
        let bytes_read = match limited.read_until(b'\n', &mut line).await {
            Ok(bytes_read) => bytes_read,
            Err(error) => {
                let error = transport_error(&error);
                self.mark_unusable();
                return Err(error);
            }
        };
        if bytes_read == 0 {
            // Signal the initially verified group before any wait/try_wait can
            // reap the leader. Surviving members then keep the PGID reserved
            // through the TERM -> KILL escalation.
            self.mark_unusable();
            return Err(Error::ProcessExited { code: None });
        }
        if bytes_read > self.limits.max_line_bytes {
            self.mark_unusable();
            return Err(Error::ResponseTooLarge);
        }
        if line.last() != Some(&b'\n') {
            self.mark_unusable();
            return Err(Error::InvalidMessage);
        }
        match decode(&line) {
            Ok(message) => Ok(message),
            Err(error) => {
                self.mark_unusable();
                Err(error)
            }
        }
    }

    async fn write_body(&mut self, body: &[u8]) -> Result<()> {
        let result = async {
            let stdin = self.stdin.as_mut().ok_or(Error::ConnectionUnusable)?;
            stdin
                .write_all(body)
                .await
                .map_err(|error| transport_error(&error))?;
            stdin.flush().await.map_err(|error| transport_error(&error))
        }
        .await;
        if result.is_err() {
            self.mark_unusable();
        }
        result
    }

    async fn deny_request(
        &mut self,
        id: &Value,
        method: &str,
        params: Option<Value>,
    ) -> Result<RequestDisposition> {
        if let Some(params) = params {
            zeroize_json_value(params);
        }
        let (body, disposition) = match method {
            "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => (
                encode_success(
                    id,
                    DecisionResponse {
                        decision: "decline",
                    },
                    self.limits.max_request_bytes,
                )?,
                RequestDisposition::Continue,
            ),
            "execCommandApproval" | "applyPatchApproval" => (
                encode_success(
                    id,
                    DecisionResponse { decision: "abort" },
                    self.limits.max_request_bytes,
                )?,
                RequestDisposition::Continue,
            ),
            "item/permissions/requestApproval" => (
                encode_success(
                    id,
                    PermissionsResponse {
                        permissions: EmptyObject {},
                    },
                    self.limits.max_request_bytes,
                )?,
                RequestDisposition::Continue,
            ),
            "mcpServer/elicitation/request" => (
                encode_success(
                    id,
                    ElicitationResponse {
                        action: "decline",
                        content: None,
                    },
                    self.limits.max_request_bytes,
                )?,
                RequestDisposition::Continue,
            ),
            "item/tool/requestUserInput" => (
                encode_failure(
                    id,
                    METHOD_NOT_FOUND,
                    "Interactive user input is unsupported",
                    self.limits.max_request_bytes,
                )?,
                RequestDisposition::Fatal,
            ),
            _ => (
                encode_failure(
                    id,
                    METHOD_NOT_FOUND,
                    "Client denies server-initiated requests",
                    self.limits.max_request_bytes,
                )?,
                RequestDisposition::Continue,
            ),
        };
        self.write_body(&body).await?;
        Ok(disposition)
    }

    fn handle_notification(&mut self, method: &str, params: Option<Value>) -> Result<()> {
        match method {
            "account/login/completed" => {
                let notification: LoginCompleted =
                    serde_json::from_value(params.ok_or(Error::InvalidMessage)?)
                        .map_err(|_| Error::InvalidMessage)?;
                let LoginCompleted {
                    login_id,
                    success,
                    error,
                } = notification;
                drop(error);
                validate_identifier(&login_id)?;
                self.push_login_notification(LoginNotification {
                    login_id: SecretString::from(login_id.to_string()),
                    success,
                })?;
            }
            "item/completed" => {
                let notification: ItemCompleted =
                    serde_json::from_value(params.ok_or(Error::InvalidMessage)?)
                        .map_err(|_| Error::InvalidMessage)?;
                validate_identifier(&notification.thread_id)?;
                validate_identifier(&notification.turn_id)?;
                if notification.item.kind == "agentMessage" {
                    let text = notification.item.text.ok_or(Error::InvalidMessage)?;
                    self.push_turn_notification(TurnNotification::Item {
                        thread_id: notification.thread_id,
                        turn_id: notification.turn_id,
                        text,
                    })?;
                }
            }
            "turn/completed" => {
                let notification: TurnCompleted =
                    serde_json::from_value(params.ok_or(Error::InvalidMessage)?)
                        .map_err(|_| Error::InvalidMessage)?;
                validate_identifier(&notification.thread_id)?;
                validate_identifier(&notification.turn.id)?;
                let final_text = notification
                    .turn
                    .items
                    .into_iter()
                    .filter(|item| item.kind == "agentMessage")
                    .filter_map(|item| item.text)
                    .next_back();
                self.push_turn_notification(TurnNotification::Completed {
                    thread_id: notification.thread_id,
                    turn_id: notification.turn.id,
                    status: notification.turn.status,
                    final_text,
                })?;
            }
            _ => {
                if let Some(params) = params {
                    zeroize_json_value(params);
                }
            }
        }
        Ok(())
    }

    fn accept_notification(&mut self, method: &str, params: Option<Value>) -> Result<()> {
        let result = self.handle_notification(method, params);
        if result.is_err() {
            self.mark_unusable();
        }
        result
    }

    fn push_login_notification(&mut self, notification: LoginNotification) -> Result<()> {
        if self.login_notifications.len() >= self.limits.max_pending_notifications {
            self.mark_unusable();
            return Err(Error::NotificationOverflow);
        }
        let byte_len = notification.byte_len();
        self.reserve_queued_bytes(byte_len)?;
        self.login_notifications.push_back(notification);
        Ok(())
    }

    fn push_turn_notification(&mut self, notification: TurnNotification) -> Result<()> {
        if self.turn_notifications.len() >= self.limits.max_pending_notifications {
            self.mark_unusable();
            return Err(Error::NotificationOverflow);
        }
        let byte_len = notification.byte_len();
        self.reserve_queued_bytes(byte_len)?;
        self.turn_notifications.push_back(notification);
        Ok(())
    }

    fn reserve_queued_bytes(&mut self, byte_len: usize) -> Result<()> {
        let Some(new_queued_bytes) = self.queued_bytes.checked_add(byte_len) else {
            self.mark_unusable();
            return Err(Error::QueuedDataOverflow);
        };
        let Some(aggregate_bytes) = new_queued_bytes.checked_add(self.retained_output_bytes) else {
            self.mark_unusable();
            return Err(Error::QueuedDataOverflow);
        };
        if aggregate_bytes > self.limits.max_queued_bytes {
            self.mark_unusable();
            return Err(Error::QueuedDataOverflow);
        }
        self.queued_bytes = new_queued_bytes;
        Ok(())
    }

    fn take_login_notification(&mut self, login_id: &str) -> Option<LoginNotification> {
        let position = self
            .login_notifications
            .iter()
            .position(|notification| notification.login_id.expose_secret() == login_id)?;
        let notification = self.login_notifications.remove(position)?;
        self.queued_bytes = self.queued_bytes.saturating_sub(notification.byte_len());
        Some(notification)
    }

    fn allocate_id(&mut self) -> Result<u64> {
        let id = self.next_id;
        self.next_id = self.next_id.checked_add(1).ok_or_else(|| {
            self.mark_unusable();
            Error::ConnectionUnusable
        })?;
        Ok(id)
    }

    fn ensure_usable(&self) -> Result<()> {
        if self.unusable || self.poisoned.load(Ordering::Acquire) {
            Err(Error::ConnectionUnusable)
        } else {
            Ok(())
        }
    }

    fn operation_guard(&self) -> Result<CancellationGuard> {
        self.ensure_usable()?;
        Ok(self
            .child
            .cancellation_guard(Arc::clone(&self.poisoned), self.limits.shutdown_timeout))
    }

    async fn settle_operation<T>(
        &mut self,
        result: Result<T>,
        mut guard: CancellationGuard,
    ) -> Result<T> {
        if result.as_ref().err().is_some_and(error_requires_teardown) {
            self.mark_unusable();
            self.abort_process().await;
        }
        guard.disarm();
        result
    }

    fn mark_unusable(&mut self) {
        self.unusable = true;
        self.poisoned.store(true, Ordering::Release);
        self.child.request_termination();
    }

    async fn abort_process(&mut self) {
        self.stdin.take();
        if self.child.teardown_arranged_or_complete() {
            return;
        }
        let _ = self
            .child
            .terminate_and_reap(self.limits.shutdown_timeout)
            .await;
    }

    async fn fail_connection<T>(&mut self, error: Error) -> Result<T> {
        self.mark_unusable();
        self.abort_process().await;
        Err(error)
    }

    async fn interrupt_and_fail<T>(&mut self, error: Error) -> Result<T> {
        self.best_effort_interrupt().await;
        self.active_turn = None;
        self.fail_connection(error).await
    }

    async fn best_effort_interrupt(&mut self) {
        let Some(active_turn) = self.active_turn.clone() else {
            return;
        };
        if self.unusable {
            return;
        }
        let Ok(id) = self.allocate_id() else {
            return;
        };
        let Ok(body) = encode_request(
            id,
            "turn/interrupt",
            &TurnInterruptParams {
                thread_id: &active_turn.thread_id,
                turn_id: &active_turn.turn_id,
            },
            self.limits.max_request_bytes,
        ) else {
            return;
        };
        let timeout = self.limits.request_timeout.min(Duration::from_millis(250));
        let _ = time::timeout(timeout, async {
            self.write_body(&body).await?;
            loop {
                match self.read_incoming().await? {
                    Incoming::Response {
                        id: response_id,
                        result,
                    } if response_id_matches(&response_id, id) && result.is_ok() => return Ok(()),
                    Incoming::Notification { method, params } => {
                        self.accept_notification(&method, params)?;
                    }
                    _ => return Err(Error::InvalidMessage),
                }
            }
        })
        .await;
    }
}

impl Drop for CodexAppServer {
    fn drop(&mut self) {
        self.stdin.take();
        if !self.child.teardown_arranged_or_complete() {
            self.child.request_termination();
        }
    }
}

fn prepare_thread_options(options: &ThreadOptions, workspace_roots: &[PathBuf]) -> Result<PathBuf> {
    validate_optional_short(options.model.as_deref(), "model is invalid")?;
    let cwd = canonical_directory(&options.cwd, "thread cwd is not a directory")?;
    if !cwd_is_allowed(&cwd, workspace_roots) {
        return Err(Error::InvalidConfiguration(
            "thread cwd is outside the workspace allowlist",
        ));
    }
    Ok(cwd)
}

fn cwd_is_allowed(cwd: &Path, workspace_roots: &[PathBuf]) -> bool {
    workspace_roots.iter().any(|root| cwd.starts_with(root))
}

fn zeroize_json_value(value: Value) {
    match value {
        Value::String(mut value) => value.zeroize(),
        Value::Array(values) => {
            for value in values {
                zeroize_json_value(value);
            }
        }
        Value::Object(values) => {
            for (mut key, value) in values {
                key.zeroize();
                zeroize_json_value(value);
            }
        }
        _ => {}
    }
}

fn path_as_str(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or(Error::InvalidConfiguration("path is not valid UTF-8"))
}

fn checked_identifier(value: String) -> Result<String> {
    validate_identifier(&value)?;
    Ok(value)
}

fn validate_identifier(value: &str) -> Result<()> {
    if value.is_empty() || value.len() > MAX_IDENTIFIER_BYTES {
        return Err(Error::InvalidMessage);
    }
    Ok(())
}

fn checked_short(value: String) -> Result<String> {
    if value.is_empty() || value.len() > MAX_SHORT_STRING_BYTES {
        return Err(Error::InvalidMessage);
    }
    Ok(value)
}

fn required_secret_identifier(value: Option<Zeroizing<String>>) -> Result<SecretString> {
    let value = value.ok_or(Error::InvalidMessage)?;
    validate_identifier(&value)?;
    Ok(SecretString::from(value.to_string()))
}

fn required_secret_short(value: Option<Zeroizing<String>>) -> Result<SecretString> {
    let value = value.ok_or(Error::InvalidMessage)?;
    if value.is_empty() || value.len() > MAX_SHORT_STRING_BYTES {
        return Err(Error::InvalidMessage);
    }
    Ok(SecretString::from(value.to_string()))
}

fn required_secret_bounded(
    value: Option<Zeroizing<String>>,
    max_bytes: usize,
) -> Result<SecretString> {
    let value = value.ok_or(Error::InvalidMessage)?;
    if value.is_empty() || value.len() > max_bytes {
        return Err(Error::InvalidMessage);
    }
    Ok(SecretString::from(value.to_string()))
}

fn validate_optional_short(value: Option<&str>, message: &'static str) -> Result<()> {
    if value.is_some_and(|value| value.is_empty() || value.len() > MAX_SHORT_STRING_BYTES) {
        return Err(Error::InvalidConfiguration(message));
    }
    Ok(())
}

fn error_requires_teardown(error: &Error) -> bool {
    !matches!(
        error,
        Error::InvalidConfiguration(_)
            | Error::RequestTooLarge
            | Error::PromptTooLarge
            | Error::Rpc { .. }
            | Error::AuthenticationFailed
            | Error::TurnInterrupted
            | Error::TurnFailed
            | Error::MissingStructuredOutput
            | Error::InvalidStructuredOutput
            | Error::NoSupportedRuntime
    )
}

fn login_result(notification: &LoginNotification) -> Result<()> {
    if notification.success {
        Ok(())
    } else {
        Err(Error::AuthenticationFailed)
    }
}

fn apply_turn_notification(
    notification: TurnNotification,
    expected_thread_id: &str,
    expected_turn_id: &str,
    latest_agent_text: &mut Option<Zeroizing<String>>,
) -> Option<TurnCompletion> {
    match notification {
        TurnNotification::Item {
            thread_id,
            turn_id,
            text,
        } if thread_id == expected_thread_id && turn_id == expected_turn_id => {
            *latest_agent_text = Some(text);
            None
        }
        TurnNotification::Completed {
            thread_id,
            turn_id,
            status,
            final_text,
        } if thread_id == expected_thread_id && turn_id == expected_turn_id => {
            Some(TurnCompletion { status, final_text })
        }
        _ => None,
    }
}

fn finish_turn<T: DeserializeOwned>(
    turn_id: String,
    completion: TurnCompletion,
    latest_agent_text: Option<Zeroizing<String>>,
) -> Result<StructuredTurn<T>> {
    match completion.status.as_str() {
        "completed" => {
            let text = completion
                .final_text
                .or(latest_agent_text)
                .ok_or(Error::MissingStructuredOutput)?;
            let output = serde_json::from_slice(text.as_bytes())
                .map_err(|_| Error::InvalidStructuredOutput)?;
            Ok(StructuredTurn { turn_id, output })
        }
        "interrupted" => Err(Error::TurnInterrupted),
        "failed" => Err(Error::TurnFailed),
        _ => Err(Error::InvalidMessage),
    }
}

#[cfg(test)]
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct InitializeParams<'a> {
    client_info: ClientInfo<'a>,
}

#[cfg(test)]
#[derive(Serialize)]
struct ClientInfo<'a> {
    name: &'a str,
    title: &'a str,
    version: &'a str,
}

#[cfg(test)]
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct InitializeResponse {
    user_agent: String,
    platform_family: String,
    platform_os: String,
    codex_home: PathBuf,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AccountReadParams {
    refresh_token: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AccountReadResponse {
    account: Option<AccountWire>,
    requires_openai_auth: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AccountWire {
    #[serde(rename = "type")]
    kind: String,
    email: Option<String>,
    plan_type: Option<String>,
    credential_source: Option<String>,
}

impl AccountWire {
    fn into_public(self) -> Result<Account> {
        checked_short(self.kind.clone())?;
        Ok(match self.kind.as_str() {
            "apiKey" => Account::ApiKey,
            "chatgpt" => Account::ChatGpt {
                email: self.email.map(checked_short).transpose()?,
                plan_type: self.plan_type.map(checked_short).transpose()?,
            },
            "amazonBedrock" => {
                let credential_source = match self.credential_source.as_deref() {
                    Some("codexManaged") => BedrockCredentialSource::CodexManaged,
                    Some("awsManaged") => BedrockCredentialSource::AwsManaged,
                    _ => return Err(Error::InvalidMessage),
                };
                Account::AmazonBedrock { credential_source }
            }
            _ => Account::Other {
                account_type: self.kind,
            },
        })
    }
}

#[derive(Serialize)]
struct ApiKeyLoginParams<'a> {
    #[serde(rename = "type")]
    kind: &'a str,
    #[serde(rename = "apiKey")]
    api_key: &'a str,
}

#[derive(Serialize)]
struct ChatGptLoginParams<'a> {
    #[serde(rename = "type")]
    kind: &'a str,
    #[serde(rename = "useHostedLoginSuccessPage")]
    use_hosted_login_success_page: bool,
    #[serde(rename = "appBrand")]
    app_brand: &'a str,
}

#[derive(Serialize)]
struct DeviceCodeLoginParams<'a> {
    #[serde(rename = "type")]
    kind: &'a str,
}

#[derive(Deserialize)]
struct LoginResponse {
    #[serde(rename = "type")]
    kind: String,
    #[serde(rename = "loginId")]
    login_id: Option<Zeroizing<String>>,
    #[serde(rename = "authUrl")]
    auth_url: Option<Zeroizing<String>>,
    #[serde(rename = "verificationUrl")]
    verification_url: Option<Zeroizing<String>>,
    #[serde(rename = "userCode")]
    user_code: Option<Zeroizing<String>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LoginCompleted {
    login_id: Zeroizing<String>,
    success: bool,
    #[serde(default)]
    error: Option<Zeroizing<String>>,
}

struct LoginNotification {
    login_id: SecretString,
    success: bool,
}

impl LoginNotification {
    fn byte_len(&self) -> usize {
        self.login_id.expose_secret().len() + std::mem::size_of::<bool>()
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ThreadStartParams<'a> {
    cwd: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<&'a str>,
    approval_policy: &'a str,
    sandbox: &'a str,
    service_name: &'a str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ThreadResumeParams<'a> {
    thread_id: &'a str,
    cwd: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<&'a str>,
    approval_policy: &'a str,
    sandbox: &'a str,
}

#[derive(Deserialize)]
struct ThreadResponse {
    thread: ThreadWire,
}

#[derive(Deserialize)]
struct ThreadWire {
    id: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TurnStartParams<'a> {
    thread_id: &'a str,
    input: &'a [TextInput<'a>],
    output_schema: &'a Value,
    approval_policy: &'a str,
    sandbox_policy: ReadOnlySandbox<'a>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    effort: Option<&'a str>,
}

#[derive(Serialize)]
struct TextInput<'a> {
    #[serde(rename = "type")]
    kind: &'a str,
    text: &'a str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ReadOnlySandbox<'a> {
    #[serde(rename = "type")]
    kind: &'a str,
    network_access: bool,
    access: RestrictedReadAccess<'a>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RestrictedReadAccess<'a> {
    #[serde(rename = "type")]
    kind: &'a str,
    include_platform_defaults: bool,
    readable_roots: [&'a str; 1],
}

#[derive(Deserialize)]
struct TurnStartResponse {
    turn: StartedTurn,
}

#[derive(Deserialize)]
struct StartedTurn {
    id: String,
    status: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ItemCompleted {
    thread_id: String,
    turn_id: String,
    item: ItemWire,
}

#[derive(Deserialize)]
struct ItemWire {
    #[serde(rename = "type")]
    kind: String,
    text: Option<Zeroizing<String>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TurnCompleted {
    thread_id: String,
    turn: CompletedTurn,
}

#[derive(Deserialize)]
struct CompletedTurn {
    id: String,
    status: String,
    #[serde(default)]
    items: Vec<ItemWire>,
}

enum TurnNotification {
    Item {
        thread_id: String,
        turn_id: String,
        text: Zeroizing<String>,
    },
    Completed {
        thread_id: String,
        turn_id: String,
        status: String,
        final_text: Option<Zeroizing<String>>,
    },
}

impl TurnNotification {
    fn byte_len(&self) -> usize {
        match self {
            Self::Item {
                thread_id,
                turn_id,
                text,
            } => thread_id.len() + turn_id.len() + text.len(),
            Self::Completed {
                thread_id,
                turn_id,
                status,
                final_text,
            } => {
                thread_id.len()
                    + turn_id.len()
                    + status.len()
                    + final_text.as_ref().map_or(0, |text| text.len())
            }
        }
    }
}

#[derive(Clone)]
struct ActiveTurn {
    thread_id: String,
    turn_id: String,
}

struct TurnCompletion {
    status: String,
    final_text: Option<Zeroizing<String>>,
}

#[derive(Serialize)]
struct DecisionResponse {
    decision: &'static str,
}

#[derive(Serialize)]
struct PermissionsResponse {
    permissions: EmptyObject,
}

#[derive(Serialize)]
struct ElicitationResponse {
    action: &'static str,
    content: Option<Value>,
}

#[derive(Serialize)]
struct EmptyObject {}

#[derive(Clone, Copy)]
enum RequestDisposition {
    Continue,
    Fatal,
}

impl RequestDisposition {
    const fn is_fatal(self) -> bool {
        matches!(self, Self::Fatal)
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TurnInterruptParams<'a> {
    thread_id: &'a str,
    turn_id: &'a str,
}

#[derive(Deserialize)]
struct EmptyResponse {}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        path::{Path, PathBuf},
        sync::{
            Arc,
            atomic::{AtomicU64, Ordering},
        },
        time::Duration,
    };

    use secrecy::SecretString;
    use serde::Deserialize;
    use serde_json::{Value, json};
    use tokio::sync::Mutex;

    use super::*;
    use crate::{AllowedEnvironment, EnvironmentKey};

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);

    struct TestWorkspace {
        base: PathBuf,
        project: PathBuf,
        state: PathBuf,
        home: PathBuf,
    }

    impl TestWorkspace {
        fn new(label: &str) -> Self {
            let sequence = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
            let base = std::env::temp_dir().join(format!(
                "dayweave-codex-unit-{label}-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&base).expect("create test base");
            let base = fs::canonicalize(base).expect("canonical test base");
            let project = base.join("project");
            let state = base.join("state");
            fs::create_dir(&project).expect("create project");
            fs::create_dir(&state).expect("create state");
            Self {
                home: base.join("codex-home"),
                base,
                project,
                state,
            }
        }

        fn scenario(&self, name: &str) {
            fs::write(self.state.join(name), []).expect("write scenario marker");
        }

        fn config(&self) -> CodexAppServerConfig {
            CodexAppServerConfig::new(fixture(), &self.home, [self.project.clone()])
                .with_environment(
                    AllowedEnvironment::new()
                        .with(EnvironmentKey::Lang, "C")
                        .with(
                            EnvironmentKey::TmpDir,
                            self.state.to_str().expect("UTF-8 state path"),
                        ),
                )
        }

        fn transcript(&self) -> Vec<Value> {
            fs::read_to_string(self.state.join("transcript.jsonl"))
                .expect("read transcript")
                .lines()
                .map(|line| serde_json::from_str(line).expect("valid transcript JSON"))
                .collect()
        }
    }

    impl Drop for TestWorkspace {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.base).expect("remove test base");
        }
    }

    fn fixture() -> PathBuf {
        fs::canonicalize(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake_app_server.sh"),
        )
        .expect("canonical fake executable")
    }

    fn message_with_method<'a>(messages: &'a [Value], method: &str) -> &'a Value {
        messages
            .iter()
            .find(|message| message.get("method").and_then(Value::as_str) == Some(method))
            .expect("method in transcript")
    }

    fn message_with_id<'a>(messages: &'a [Value], id: &str) -> &'a Value {
        messages
            .iter()
            .find(|message| message.get("id").and_then(Value::as_str) == Some(id))
            .expect("response id in transcript")
    }

    fn contains_json_string(value: &Value, expected: &str) -> bool {
        match value {
            Value::String(value) => value == expected,
            Value::Array(values) => values
                .iter()
                .any(|value| contains_json_string(value, expected)),
            Value::Object(values) => values
                .values()
                .any(|value| contains_json_string(value, expected)),
            _ => false,
        }
    }

    async fn prepared_thread(server: &mut CodexAppServer, project: &Path) -> ThreadHandle {
        server.read_account().await.expect("read account");
        let thread = server
            .start_thread(&ThreadOptions::new(project))
            .await
            .expect("start thread");
        server
            .resume_thread(&thread, &ThreadOptions::new(project))
            .await
            .expect("resume thread")
    }

    #[derive(Debug, Deserialize, Eq, PartialEq)]
    struct Answer {
        answer: u32,
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn test_transport_enforces_home_mode_allowlist_and_structural_sandbox() {
        let workspace = TestWorkspace::new("structure");
        let outside = workspace.base.join("outside");
        fs::create_dir(&outside).expect("outside directory");
        fs::write(outside.join("sentinel-secret"), b"outside").expect("outside sentinel");

        let mut server = CodexAppServer::spawn_test_runtime(&workspace.config())
            .await
            .expect("start test transport");
        assert_eq!(
            fs::metadata(&workspace.home)
                .expect("home metadata")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert!(
            fs::read_dir(&workspace.home)
                .expect("inspect home")
                .next()
                .is_none(),
            "new app-owned CODEX_HOME starts empty"
        );

        assert!(matches!(
            server.start_thread(&ThreadOptions::new(&outside)).await,
            Err(Error::InvalidConfiguration(_))
        ));
        let thread = prepared_thread(&mut server, &workspace.project).await;
        let request = StructuredTurnRequest::new(
            SecretString::from("private planner prompt".to_owned()),
            json!({"type":"object","properties":{"answer":{"type":"integer"}}}),
        );
        let turn = server
            .run_structured_turn::<Answer>(&thread, &request)
            .await
            .expect("complete structured turn");
        assert_eq!(turn.output(), &Answer { answer: 42 });
        assert!(
            fs::read_dir(&workspace.home)
                .expect("reinspect home")
                .next()
                .is_none(),
            "test transport may not leak uncontrolled files into CODEX_HOME"
        );
        server.shutdown().await.expect("clean shutdown");

        let messages = workspace.transcript();
        assert_eq!(
            message_with_method(&messages, "initialize")["params"],
            json!({
                "clientInfo": {
                    "name": "dayweave",
                    "title": "DayWeave",
                    "version": env!("CARGO_PKG_VERSION")
                }
            })
        );
        assert_eq!(
            message_with_method(&messages, "initialized"),
            &json!({"method": "initialized"})
        );
        assert_eq!(
            message_with_method(&messages, "account/read")["params"],
            json!({"refreshToken": false})
        );
        assert_eq!(
            message_with_method(&messages, "thread/start")["params"],
            json!({
                "cwd": workspace.project.to_str().expect("UTF-8 project"),
                "approvalPolicy": "never",
                "sandbox": "read-only",
                "serviceName": "dayweave"
            })
        );
        assert_eq!(
            message_with_method(&messages, "thread/resume")["params"],
            json!({
                "threadId": "thread-1",
                "cwd": workspace.project.to_str().expect("UTF-8 project"),
                "approvalPolicy": "never",
                "sandbox": "read-only"
            })
        );
        let turn_start = message_with_method(&messages, "turn/start");
        assert_eq!(turn_start["params"]["threadId"], json!("thread-1"));
        assert_eq!(turn_start["params"]["approvalPolicy"], json!("never"));
        assert_eq!(
            turn_start["params"]["input"],
            json!([{"type": "text", "text": "private planner prompt"}])
        );
        assert_eq!(
            turn_start["params"]["outputSchema"],
            json!({"type":"object","properties":{"answer":{"type":"integer"}}})
        );
        assert_eq!(
            turn_start["params"]["sandboxPolicy"],
            json!({
                "type": "readOnly",
                "networkAccess": false,
                "access": {
                    "type": "restricted",
                    "includePlatformDefaults": false,
                    "readableRoots": [workspace.project.to_str().expect("UTF-8 project")]
                }
            })
        );
        assert_ne!(
            turn_start["params"]["sandboxPolicy"]["access"]["readableRoots"],
            json!([outside.to_str().expect("UTF-8 outside")])
        );
        assert!(!contains_json_string(
            turn_start,
            outside.to_str().expect("UTF-8 outside")
        ));
        assert!(!contains_json_string(turn_start, "outside"));
        assert_eq!(
            message_with_id(&messages, "command-approval"),
            &json!({"id": "command-approval", "result": {"decision": "decline"}})
        );
        assert_eq!(
            message_with_id(&messages, "file-approval"),
            &json!({"id": "file-approval", "result": {"decision": "decline"}})
        );
        assert_eq!(
            message_with_id(&messages, "permissions-approval"),
            &json!({"id": "permissions-approval", "result": {"permissions": {}}})
        );
        assert_eq!(
            message_with_id(&messages, "mcp-elicitation"),
            &json!({
                "id": "mcp-elicitation",
                "result": {"action": "decline", "content": null}
            })
        );
        assert_eq!(
            message_with_id(&messages, "legacy-exec")["result"]["decision"],
            json!("abort")
        );
        assert_eq!(
            message_with_id(&messages, "legacy-patch")["result"]["decision"],
            json!("abort")
        );
        assert_eq!(
            message_with_id(&messages, "unknown-request"),
            &json!({
                "id": "unknown-request",
                "error": {
                    "code": -32601,
                    "message": "Client denies server-initiated requests"
                }
            })
        );
    }

    #[tokio::test]
    async fn user_input_is_never_synthesized_and_fatally_interrupts() {
        let workspace = TestWorkspace::new("user-input");
        workspace.scenario("user-input-flow");
        let mut server = CodexAppServer::spawn_test_runtime(&workspace.config())
            .await
            .expect("start test transport");
        let thread = prepared_thread(&mut server, &workspace.project).await;
        let request = StructuredTurnRequest::new(
            SecretString::from("private planner prompt".to_owned()),
            json!({"type":"object"}),
        );
        assert!(matches!(
            server.run_structured_turn::<Value>(&thread, &request).await,
            Err(Error::InvalidMessage)
        ));
        assert!(matches!(
            server.read_account().await,
            Err(Error::ConnectionUnusable)
        ));

        let messages = workspace.transcript();
        let denial = message_with_id(&messages, "user-input");
        assert_eq!(denial["error"]["code"], json!(-32601));
        assert!(denial.get("result").is_none());
        assert!(denial.to_string().find("answers").is_none());
        let interrupt = message_with_method(&messages, "turn/interrupt");
        assert_eq!(interrupt["params"]["threadId"], json!("thread-1"));
        assert_eq!(interrupt["params"]["turnId"], json!("turn-1"));
    }

    #[tokio::test]
    async fn outer_timeout_poisoning_kills_descendants() {
        let workspace = TestWorkspace::new("outer-timeout");
        workspace.scenario("timeout-flow");
        let mut server = CodexAppServer::spawn_test_runtime(&workspace.config())
            .await
            .expect("start test transport");
        assert!(
            tokio::time::timeout(Duration::from_millis(150), server.read_account())
                .await
                .is_err()
        );
        assert!(matches!(
            server.read_account().await,
            Err(Error::ConnectionUnusable)
        ));
        tokio::time::sleep(Duration::from_millis(1_200)).await;
        assert!(!workspace.state.join("grandchild-survived").exists());
    }

    #[tokio::test]
    async fn task_abort_poisoning_kills_descendants() {
        let workspace = TestWorkspace::new("task-abort");
        workspace.scenario("timeout-flow");
        let server = CodexAppServer::spawn_test_runtime(&workspace.config())
            .await
            .expect("start test transport");
        let shared = Arc::new(Mutex::new(server));
        let operation_server = Arc::clone(&shared);
        let operation =
            tokio::spawn(async move { operation_server.lock().await.read_account().await });
        tokio::time::sleep(Duration::from_millis(150)).await;
        operation.abort();
        let Err(join_error) = operation.await else {
            panic!("task should be cancelled");
        };
        assert!(join_error.is_cancelled());
        assert!(matches!(
            shared.lock().await.read_account().await,
            Err(Error::ConnectionUnusable)
        ));
        tokio::time::sleep(Duration::from_millis(1_200)).await;
        assert!(!workspace.state.join("grandchild-survived").exists());
    }

    #[tokio::test]
    async fn abort_during_fatal_cleanup_hands_off_to_detached_reaper() {
        let workspace = TestWorkspace::new("abort-during-cleanup");
        workspace.scenario("bad-turn-start-flow");
        let mut server = CodexAppServer::spawn_test_runtime(&workspace.config())
            .await
            .expect("start test transport");
        let thread = prepared_thread(&mut server, &workspace.project).await;
        let shared = Arc::new(Mutex::new(server));
        let operation_server = Arc::clone(&shared);
        let operation = tokio::spawn(async move {
            let request = StructuredTurnRequest::new(
                SecretString::from("private planner prompt".to_owned()),
                json!({"type":"object"}),
            );
            operation_server
                .lock()
                .await
                .run_structured_turn::<Value>(&thread, &request)
                .await
        });

        tokio::time::timeout(Duration::from_secs(1), async {
            while !workspace.state.join("fatal-response-sent").exists() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("fake sent the semantic failure");
        tokio::time::sleep(Duration::from_millis(20)).await;
        operation.abort();
        let Err(join_error) = operation.await else {
            panic!("task should be cancelled during cleanup");
        };
        assert!(join_error.is_cancelled());
        assert!(matches!(
            shared.lock().await.read_account().await,
            Err(Error::ConnectionUnusable)
        ));
        tokio::time::sleep(Duration::from_millis(1_200)).await;
        assert!(!workspace.state.join("grandchild-survived").exists());
    }

    #[tokio::test]
    async fn exited_leader_cannot_leave_residual_process_group() {
        let workspace = TestWorkspace::new("exited-leader");
        workspace.scenario("clean-exit-grandchild-flow");
        let mut server = CodexAppServer::spawn_test_runtime(&workspace.config())
            .await
            .expect("start test transport");
        assert!(server.read_account().await.is_err());
        assert!(matches!(
            server.read_account().await,
            Err(Error::ConnectionUnusable)
        ));
        tokio::time::sleep(Duration::from_millis(1_200)).await;
        assert!(!workspace.state.join("grandchild-survived").exists());
    }

    #[tokio::test]
    async fn resume_thread_id_mismatch_is_fatal() {
        let workspace = TestWorkspace::new("resume-mismatch");
        workspace.scenario("resume-mismatch-flow");
        let mut server = CodexAppServer::spawn_test_runtime(&workspace.config())
            .await
            .expect("start test transport");
        server.read_account().await.expect("read account");
        let thread = server
            .start_thread(&ThreadOptions::new(&workspace.project))
            .await
            .expect("start thread");
        assert!(matches!(
            server
                .resume_thread(&thread, &ThreadOptions::new(&workspace.project))
                .await,
            Err(Error::InvalidMessage)
        ));
        assert!(matches!(
            server.read_account().await,
            Err(Error::ConnectionUnusable)
        ));
    }

    #[tokio::test]
    async fn invalid_turn_start_status_is_fatal_and_reaps_descendants() {
        let workspace = TestWorkspace::new("turn-status");
        workspace.scenario("bad-turn-start-flow");
        let mut server = CodexAppServer::spawn_test_runtime(&workspace.config())
            .await
            .expect("start test transport");
        let thread = prepared_thread(&mut server, &workspace.project).await;
        let request = StructuredTurnRequest::new(
            SecretString::from("private planner prompt".to_owned()),
            json!({"type":"object"}),
        );
        assert!(matches!(
            server.run_structured_turn::<Value>(&thread, &request).await,
            Err(Error::InvalidMessage)
        ));
        assert!(matches!(
            server.read_account().await,
            Err(Error::ConnectionUnusable)
        ));
        tokio::time::sleep(Duration::from_millis(1_200)).await;
        assert!(!workspace.state.join("grandchild-survived").exists());
    }

    #[tokio::test]
    async fn aggregate_queued_output_budget_is_fatal() {
        let workspace = TestWorkspace::new("queue-budget");
        workspace.scenario("queued-overflow-flow");
        let limits = ProtocolLimits {
            max_queued_bytes: 128,
            ..ProtocolLimits::default()
        };
        let config = workspace.config().with_limits(limits);
        let mut server = CodexAppServer::spawn_test_runtime(&config)
            .await
            .expect("start test transport");
        let thread = prepared_thread(&mut server, &workspace.project).await;
        let request = StructuredTurnRequest::new(
            SecretString::from("private planner prompt".to_owned()),
            json!({"type":"object"}),
        );
        assert!(matches!(
            server.run_structured_turn::<Value>(&thread, &request).await,
            Err(Error::QueuedDataOverflow)
        ));
        assert!(matches!(
            server.read_account().await,
            Err(Error::ConnectionUnusable)
        ));
    }

    #[tokio::test]
    async fn null_login_id_is_fatal_instead_of_entering_the_queue() {
        let workspace = TestWorkspace::new("null-login");
        workspace.scenario("null-login-flow");
        let mut server = CodexAppServer::spawn_test_runtime(&workspace.config())
            .await
            .expect("start test transport");
        assert!(matches!(
            server.read_account().await,
            Err(Error::InvalidMessage)
        ));
        assert!(matches!(
            server.read_account().await,
            Err(Error::ConnectionUnusable)
        ));
    }

    #[test]
    fn bedrock_account_requires_current_credential_source_field() {
        let legacy: AccountWire = serde_json::from_value(json!({
            "type": "amazonBedrock",
            "usesCodexManagedCredentials": true
        }))
        .expect("decode legacy shape");
        assert!(matches!(legacy.into_public(), Err(Error::InvalidMessage)));

        let current: AccountWire = serde_json::from_value(json!({
            "type": "amazonBedrock",
            "credentialSource": "codexManaged"
        }))
        .expect("decode current shape");
        let account = current.into_public().expect("current shape accepted");
        assert!(matches!(
            account,
            Account::AmazonBedrock {
                credential_source: BedrockCredentialSource::CodexManaged
            }
        ));
    }
}
