# Published MCP OAuth with Auth0

DayWeave implements an OAuth 2.1 resource-server boundary for published MCP
clients. It is disabled by default and separate from REST device credentials
and native `dw_mc1_` MCP credentials. Auth0 is the authorization server;
DayWeave does not implement authorization, token issuance, refresh, or client
secret storage.

Do not enable this surface until the tenant and ingress have passed the
preflight below. Never commit access tokens, refresh tokens, Auth0 management
tokens, client secrets, tenant exports, or user data. The configuration values
accepted by DayWeave are public identifiers and exact allowlists only.

Primary references:

- [OpenAI MCP authentication](https://developers.openai.com/plugins/build/auth)
- [MCP authorization specification](https://modelcontextprotocol.io/specification/2025-11-25/basic/authorization)
- [Auth0 Auth for MCP](https://auth0.com/ai/docs/mcp/intro/overview)
- [Auth0 access-token profiles](https://auth0.com/docs/secure/tokens/access-tokens/access-token-profiles)
- [Auth0 Resource Parameter Compatibility Profile](https://auth0.com/ai/docs/mcp/guides/resource-param-compatibility-profile)
- [Auth0 MCP client registration](https://auth0.com/ai/docs/mcp/guides/registering-your-mcp-client-application)
- [Auth0 Dynamic Client Registration](https://auth0.com/docs/get-started/applications/dynamic-client-registration)

## Fixed security contract

When enabled, DayWeave publishes the same RFC 9728 protected-resource document
at both paths:

- `/.well-known/oauth-protected-resource`
- `/.well-known/oauth-protected-resource/mcp`

The canonical resource is the exact public `https://.../mcp` URL from
configuration. Challenges and metadata never use `Host`, forwarding headers,
JWT headers, claims, or request parameters. An unauthenticated request receives
`401` with a `Bearer resource_metadata="..."` challenge. A tool call lacking a
scope receives an MCP tool error with `_meta["mcp/www_authenticate"]` and no
schedule or proposal call.

The three public scopes are:

- `schedule:read` for schedule/search/explanation/conflict tools
- `schedule:simulate` for side-effect-free simulations
- `suggestions:submit` for reviewable Suggestions Inbox proposals

OAuth clients see all tools with one exact OAuth scope on each tool so they can
request step-up consent. Native clients retain their scope-filtered catalog and
do not receive OAuth security schemes.

Every OAuth request must be a bounded compact JWS with an `at+jwt` header,
`RS256`, and a bounded `kid`. DayWeave obtains keys only from the same-origin
Auth0 JWKS URL derived from the configured issuer. Redirects are disabled and
responses must have exactly one parseable `application/json` or
`application/jwk-set+json` content type before their body is accepted. Response
bytes, key counts, RSA components, certificate metadata, refreshes, stale use,
and unknown-key refreshes are bounded. Token-controlled `jku`, `jwk`,
`x5u`, certificate keys, critical headers, and unknown header extensions are
rejected.

The signature and exact `iss`, sole `aud`, owner `sub`, allowed `client_id`,
`exp`, `iat`, and `scope` claims are checked on every request. Token lifetime is
limited to one hour with 60 seconds of clock skew. `nbf` is optional under RFC
9068/Auth0 and is validated with the same skew when present. The legacy Auth0
`azp` claim is never accepted as a substitute for `client_id`.

## Auth0 tenant preparation

Perform these steps in the Auth0 dashboard or reviewed infrastructure code.
They are operator actions; this repository does not make management API calls.

1. Enable Auth0's Resource Parameter Compatibility Profile so MCP clients can
   send RFC 8707 `resource` rather than an Auth0-specific audience parameter.
2. Create one Auth0 API whose identifier is byte-for-byte identical to
   `DAYWEAVE_MCP_OAUTH_RESOURCE`, including the `/mcp` path and no trailing
   slash. Configure `RS256`, the RFC 9068 access-token profile, an access-token
   lifetime of at most 3,600 seconds, and the three DayWeave scopes.
3. Enable Auth0's **Include Signing Algorithms in JWKS** option. DayWeave
   requires each accepted JWK to declare `alg: RS256`; this avoids algorithm
   ambiguity. It accepts bounded Auth0 `x5c`, `x5t`, `x5t#S256`, and
   `key_ops: ["verify"]` metadata but ignores certificate material and verifies
   only the pinned RSA `n`/`e` components.
4. Configure the personal login connection and consent policy. Obtain the
   owner's exact Auth0 `sub` without logging the full token and set it as the
   only `DAYWEAVE_MCP_OAUTH_OWNER_SUBJECT`.
5. Avoid requesting OIDC profile scopes for this API. Preflight must show a
   sole audience equal to the MCP resource, a `client_id` claim, the expected
   `sub`, `iat`, `exp`, and `scope`, and no additional audience such as
   `/userinfo`. `nbf` may be absent.

Auth0 documents manual Client ID Metadata Document registration as the
preferred production MCP option. Register OpenAI's stable ChatGPT CIMD client
identifier and allow exactly:

```text
https://chatgpt.com/oauth/client.json
```

Use that same exact value in `DAYWEAVE_MCP_OAUTH_CLIENT_IDS`. Never use a
wildcard, hostname suffix, or unverified redirect URL.

Dynamic Client Registration is an alternative only when CIMD cannot be used.
Auth0 DCR is open registration when enabled and is disabled by default. If it
is unavoidable, first configure third-party API permissions, connection and
tenant ACL restrictions; enable it only for a controlled registration window;
record the generated `tpc_...` client ID; add that exact ID to DayWeave's
allowlist; then disable open DCR and reconnect the client. Do not commit any
returned client secret. A token from an unlisted DCR client fails closed.

## DayWeave configuration and activation

OAuth is off unless `DAYWEAVE_MCP_OAUTH_ENABLED=true` exactly. Supplying any
subordinate OAuth setting while it is off is a startup error. Enabling it also
requires `DAYWEAVE_AUTH_MODE=credential_only`, PostgreSQL, and no static API
token. This prevents a published OAuth endpoint from silently retaining a
legacy bearer fallback.

Required public settings are:

```text
DAYWEAVE_MCP_OAUTH_ENABLED=true
DAYWEAVE_MCP_OAUTH_RESOURCE=https://api.example.test/mcp
DAYWEAVE_MCP_OAUTH_ISSUER=https://tenant.eu.auth0.com/
DAYWEAVE_MCP_OAUTH_OWNER_SUBJECT=auth0|replace-with-exact-owner-sub
DAYWEAVE_MCP_OAUTH_CLIENT_IDS=https://chatgpt.com/oauth/client.json
DAYWEAVE_MCP_OAUTH_ALLOWED_ORIGINS=https://chatgpt.com
```

The issuer must be a canonical public HTTPS domain at its root with no explicit
port; IP literals, `localhost`, `.localhost`, `.local`, and single-label hosts
are rejected before the derived JWKS client is constructed. The resource is
the external tunnel identity including the exact `/mcp` path. It is deliberately
not inferred from `DAYWEAVE_PUBLIC_BASE_URL`, the request `Host`, or forwarding
headers, so changing the tunnel requires a reviewed Auth0 API identifier and
resource-setting change together.

`DAYWEAVE_MCP_OAUTH_ALLOWED_ORIGINS` is a second allowlist. Every listed OAuth
origin must also appear exactly in `DAYWEAVE_MCP_ALLOWED_ORIGINS`; an incoming
Origin must pass both lists. Origins must be canonical HTTPS origins without a
path, query, fragment, credentials, or trailing slash.

The base Compose file passes only the disabled flag. This is intentional: blank
subordinate variables would violate disabled-mode validation. After putting the
reviewed public values in the VM's protected deployment environment, activate
the explicit overlay:

```sh
docker compose -f deploy/compose.yaml -f deploy/compose.mcp-oauth.yaml config
docker compose -f deploy/compose.yaml -f deploy/compose.mcp-oauth.yaml up -d
```

The overlay uses required-variable checks, explicitly forces
`DAYWEAVE_AUTH_MODE=credential_only`, and clears `DAYWEAVE_API_TOKEN`, so a
missing value or retained legacy authority cannot reach container startup. Do
not paste credential values into either file or the command line.

## Preflight and rollback

Before registering the connector in ChatGPT, verify all of the following:

1. Both protected-resource paths return the same document and exact canonical
   resource/issuer even with a hostile `Host` header.
2. An unauthenticated `/mcp` request returns the configured path-specific
   `resource_metadata` challenge.
3. A newly issued tenant token has `typ=at+jwt`, `alg=RS256`, a known `kid`, the
   sole MCP audience, exact owner subject, stable allowed client ID, required
   scopes, `iat`, and an expiry no more than one hour later. If any assertion
   differs, leave OAuth disabled; do not weaken validation to fit a token.
4. A token for another subject, client, or audience and an expired/not-yet-valid
   token all receive `401`; a missing tool scope returns the scoped MCP
   challenge without executing the tool.
5. A `dw_mc1_` token never causes a JWKS fetch, an OAuth JWT is rejected by REST,
   and a browser Origin outside either allowlist is rejected.
6. JWKS rotation succeeds while redirects, unknown-key storms, oversized sets,
   ambiguous keys, and dynamic key URLs fail closed.

To roll back, remove the OAuth overlay, set
`DAYWEAVE_MCP_OAUTH_ENABLED=false`, remove every subordinate OAuth setting from
the process environment, and restart. Both metadata routes then disappear and
native MCP behavior remains unchanged.

## Known product release gate

OAuth authentication does not itself make schedule data available. The current
production dependency graph still installs unavailable schedule-query and
simulation ports for MCP. Published ChatGPT/Codex access therefore remains a
product release blocker until those ports are wired and independently audited;
do not claim live schedule access based only on a successful OAuth handshake.
