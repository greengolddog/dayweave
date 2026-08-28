# DayWeave for macOS

The native SwiftUI client keeps the local planner usable without a server. A
new production profile starts with an empty plan and restores the encrypted
snapshot synchronously before exposing actions; preview fixtures are used only
by `PlannerStore.preview` and tests.

## Suggestions API

Open **Settings → DayWeave suggestions API** and provide:

- the server root URL, such as `https://dayweave.example.com` or the local
  development endpoint `http://127.0.0.1:8787`; the app appends
  `/v1/suggestions` itself;
- a bearer token configured on the DayWeave API.

Remote URLs must use HTTPS. Plain HTTP is limited to loopback development, and
redirects are not followed so a credential cannot be redirected to another
origin. The base URL is ordinary configuration stored in `UserDefaults`. The
bearer token is stored as a device-only generic password in the macOS Keychain;
it is never added to the Codable planner snapshot or application messages.

The Inbox fetches pending proposals and supports refresh, edit, accept, and
reject with the proposal's optimistic `expected_revision`. Remote proposals
remain a separate, in-memory review feed. Accepting one records the decision at
the API and intentionally does **not** create or mutate a schedule block. Local
suggestions and all local planning remain available when the API is absent or
offline, with the last request state shown in the Inbox.

## Verification

Use a full Xcode Swift toolchain to execute the test bundle:

```sh
swift build --package-path apps/macos -Xswiftc -warnings-as-errors
swift test --package-path apps/macos -Xswiftc -warnings-as-errors
```

The API tests use a deterministic `URLProtocol` transport and injected token
stores, so they require neither a live server nor access to the user's
Keychain. Coverage includes contract decoding, authenticated request shape,
revision-guarded actions, structured errors, configuration separation,
offline behavior, restore-failure mutation gating, and the invariant that a
remote approval leaves the schedule unchanged.
