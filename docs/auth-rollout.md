# Durable authentication rollout

The server supports three explicit authentication modes. `legacy_static` is the
backwards-compatible default and accepts only the configured static bearer.
`hybrid` accepts both the static bearer and durable device/native-MCP
credentials.
`credential_only` accepts only durable credentials and refuses to start when a
non-empty static token is configured. Hybrid and credential-only modes require
PostgreSQL.

This is a migration procedure, not a reason to place credentials in Git, shell
history, CI output, tickets, or logs. All examples below name environment
variables but intentionally contain no credential values.

## Credential contract

- Device access: `dw_da1_` plus 32 random bytes encoded as unpadded URL-safe
  base64; valid for at most 15 minutes.
- Device refresh: `dw_dr1_` plus independent random material; 30-day idle and
  180-day absolute session bounds.
- One-time enrollment: `dw_en1_` plus independent random material; valid for at
  most 10 minutes.
- Native MCP client: `dw_mc1_` plus independent random material; 90-day default
  and 365-day maximum.

The enrollment initiator generates the enrollment UUID and credential with the
operating-system CSPRNG, then durably journals the exact creation request before
network I/O. The server stores only its domain-separated digest. It returns
`201 Created` for the first insert and `200 OK` only when an identical semantic
request recovers the same still-pending, unexpired creation after response
loss. Both responses echo the proposed identifier and enrollment credential
and carry `Cache-Control: no-store` and `Pragma: no-cache`; a changed tuple,
consumed or expired enrollment, or tenant collision returns a conflict.

The server generates native MCP credentials with the operating-system CSPRNG
and returns each plaintext once. Device clients also generate and durably
journal a session UUID and each next access/refresh pair before enrollment
consumption or refresh. The server never returns those device-generated session
plaintexts.

Enrollment and refresh are exact-retry operations. If a response is lost after
commit, the same enrollment token/session UUID/access/refresh tuple or the same
old refresh/next access/next refresh tuple returns the committed metadata.
Another tuple loses and receives the same redacted authentication failure as an
invalid credential. The replay window advances after the next successful
rotation.

The device client contract is version `2`; it adds the REST-only
`schedule_publish` capability. Existing version-1 device rows remain usable
with their exact old scopes, but database and application constraints forbid
`schedule_publish` on a version-1 enrollment/session and refresh never widens a
session. A device that needs publication must re-enroll on version 2. Native MCP
remains contract version `1` and is limited to its unchanged MCP-only scope set.
Unknown contract versions, malformed client version/capability metadata,
duplicate scopes, REST scopes on an MCP client, and MCP-only submission scope
on a device session fail closed.

## API sequence

All management endpoints below are under `/v1` and require their named scope.

1. In hybrid mode, generate an enrollment `id` and independent `dw_en1_`
   credential. Journal the exact request before calling `POST
   /auth/device-enrollments` with those fields, a stable `client_instance_id`,
   `client_kind`, label, desired REST scopes, `client_contract_version: 2`,
   client version, and capabilities. It requires `auth_sessions_write`. Retry
   an ambiguous result only with the exact journaled request; discard the
   journal only after validating the status and exact echoed fields.
2. If a separate administrator initiated enrollment, transfer the credential
   out of band to the intended device. The device journals its proposed session
   UUID and new access/refresh credentials, then calls `POST
   /auth/device-enrollments/consume` with the enrollment credential as bearer
   authentication.
3. The device journals every next pair before calling `POST
   /auth/sessions/refresh` with the current refresh credential as bearer
   authentication. It atomically replaces its secure-store pair only after a
   successful response; an ambiguous network result is retried with the exact
   journaled tuple.
4. Inspect active devices using `GET /auth/sessions` (`auth_sessions_read`) and
   revoke one using `DELETE /auth/sessions/{id}`
   (`auth_sessions_write`).
5. Create a first-party or native MCP credential using `POST
   /auth/mcp-clients`, inspect it using `GET /auth/mcp-clients`, and revoke it
   using `DELETE
   /auth/mcp-clients/{id}`. Creation returns the MCP credential once. Requested
   scopes are limited to `schedule_read`, `schedule_simulate`, and
   `suggestions_submit`. A browser Origin must match both the global server
   allowlist and the exact per-client origin list.

### Native active-device inventory

The macOS and Android account surfaces read `GET /auth/sessions` only through
their coordinated authenticated-request path. They identify the current device
by exact comparison with the session UUID and client-instance UUID in the local
durable-auth envelope, plus the expected native client kind; labels are
presentation only and never authority. Inventory metadata remains memory-only,
is cleared at the privacy or credential-binding boundary, and is read-only
whenever its last refresh is stale, failed, or its exact current row lacks
`auth_sessions_write`.

A remote-device revoke requires an explicit, binding-bound confirmation and
must never delete the current device's local credential envelope. After an
ambiguous delete or a not-found response, the client re-lists sessions and
reports success only when that exact remote UUID is authoritatively absent.
The current device continues to use the stricter revoke-first sign-out flow:
local credential and API-bound cache removal occurs only through its dedicated
cross-store teardown choreography.

One owner may have at most 16 active refreshable device sessions and 16 live,
unconsumed device enrollments. The PostgreSQL repository serializes admission
per workspace and user. Exact response-loss replays remain valid at capacity,
and a same-installation replacement revokes its prior session before counting
the replacement. A distinct request beyond either cap returns `409`; access
token expiry alone does not release a refreshable-session slot. Session listing
never truncates authority: historical state above the cap fails closed instead
of hiding a revocable session.

### Published ChatGPT/Codex MCP account linking

The `dw_mc1_` credential remains a native bearer for first-party and local MCP
clients. Published clients use a separate, disabled-by-default Auth0 OAuth 2.1
resource-server boundary. Follow [`mcp-oauth.md`](mcp-oauth.md) for its exact
metadata, token, scope, allowlist, deployment, preflight, and rollback contract.
Google OAuth is unrelated provider-client authorization and is never reused as
the DayWeave authorization server.

REST rejects MCP credentials, and MCP rejects device credentials. Static
credentials work in both audiences only while legacy or hybrid mode is active.
The `dw_` namespace is forbidden for configured static tokens. In durable
modes, a token in that namespace is handled only as the exact durable kind and
is never retried against the static-token authenticator.

## Cutover checklist

1. Back up PostgreSQL, restore it in isolation, and apply migrations through
   `0013_schedule_seal_and_mcp_submission.sql` before changing authentication
   mode.
2. Deploy in `legacy_static`; verify existing clients and inspect migration and
   audit health.
3. Set `DAYWEAVE_AUTH_MODE=hybrid` while retaining the existing static token.
   Do not use hybrid mode to activate any otherwise gated external-effect or
   sensitive-data workflow.
4. Enroll macOS and Android, create narrow MCP clients, verify scope denials,
   exact retry, list/revoke, and device/MCP audience rejection. Revoke test
   credentials.
5. Audit any session, enrollment, or MCP rows created by the earlier foundation
   migration. The following two queries must each return zero rows before a
   deploy or upgrade; resolve an over-cap owner explicitly rather than deleting
   or truncating authority automatically:

   ```sql
   SELECT workspace_id, user_id, count(*) AS active_refreshable_sessions
   FROM sessions
   WHERE auth_version = 1
     AND revoked_at IS NULL
     AND refresh_idle_expires_at > CURRENT_TIMESTAMP
     AND absolute_expires_at > CURRENT_TIMESTAMP
   GROUP BY workspace_id, user_id
   HAVING count(*) > 16;

   SELECT workspace_id, user_id, count(*) AS live_pending_enrollments
   FROM device_enrollments
   WHERE consumed_at IS NULL
     AND revoked_at IS NULL
     AND expires_at > CURRENT_TIMESTAMP
   GROUP BY workspace_id, user_id
   HAVING count(*) > 16;
   ```

   Reissue rows carrying a scope that is invalid for their audience, then
   validate `sessions_v1_runtime_shape_check`,
   `sessions_schedule_publish_contract_check`,
   `device_enrollments_runtime_scopes_check`,
   `device_enrollments_schedule_publish_contract_check`,
   `mcp_clients_v1_runtime_shape_check`, and
   `mcp_clients_contract_scopes_check`.
6. Confirm every device can refresh after restart and after an intentionally
   dropped response. Keep a recoverable offline copy of the static bootstrap
   credential only until this validation is complete.
7. Set `DAYWEAVE_AUTH_MODE=credential_only`, remove the static token value from
   the runtime secret source, restart, and verify the old static bearer is
   rejected by REST and MCP.
8. Retain content-free audit rows and monitoring evidence. Destroy temporary
   plaintext transfers and rotate any credential whose handling is uncertain.

Rollback before credential-only is simply a reviewed return to
`legacy_static`. After credential-only, a rollback restores the static bearer
as an active authority and therefore requires explicit incident/change
approval; it must not happen automatically.

## Remaining release gates

The server slice does not configure ingress/per-principal rate limiting or
suspicious-authentication alerts. The first-party clients implement the exact
journal/atomic-store protocol, but it still requires independent audit and a
real-device cutover rehearsal. Published ChatGPT/Codex account linking remains
blocked until the documented Auth0/tunnel activation preflight, a
deployed end-to-end schedule read and simulation rehearsal, and an independent
security review pass. The PostgreSQL schedule publication/query/simulation and
transactional proposal-submission ports are wired; a deployment without
PostgreSQL still fails closed with unavailable adapters.
Until the remaining gates and a real-device cutover rehearsal pass, keep the
deployment classified as a development artifact.
