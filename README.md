# DayWeave

DayWeave is a private, AI-native day planner for macOS and Android. It combines
fixed calendar events, flexible tasks, habits, routines, goals, breaks, and
deeply nested projects, then composes a realistic schedule from duration,
constraints, energy, priority, and live context.

The deterministic scheduling engine works offline. AI handles natural-language
capture, collaborative planning, explanations, estimates, reviews, and
suggestions; it cannot bypass safety or external-change confirmation rules.

## Repository map

- `crates/dayweave-core`: portable domain model and scheduling engine
- `server/dayweave-api`: private sync, integration, AI, and MCP service
- `apps/macos`: native SwiftUI application
- `apps/android`: native Jetpack Compose application
- `integrations/dayweave`: Codex plugin, MCP server registration, and skill
- `deploy`: local and Nebius deployment assets
- `docs`: decisions, architecture, security, setup, operations, and user guides

The complete agreed product scope is preserved in
[`docs/discovery-answers.md`](docs/discovery-answers.md).
The implemented controls and explicit production security gates are documented
in [`docs/security.md`](docs/security.md).
The deterministic preview request and strict canonical scheduling metadata are
documented in [`docs/scheduling-api.md`](docs/scheduling-api.md).

## Status

Active implementation. The private repository is intentionally unlicensed for
public reuse; see `LICENSE.md`.
