# DayWeave connection recovery

Use this reference only when the DayWeave MCP tools are unavailable or reject
authentication.

## Local Codex development

The checked-in plugin connects to `http://127.0.0.1:8787/mcp` and reads its
bearer credential from `DAYWEAVE_MCP_TOKEN`. The variable must contain a
separately scoped DayWeave MCP credential, not a REST device access token.

Keep the value in the local process environment or an operating-system secret
store. Never put it in this repository, a plugin file, a prompt, chat history,
shell history, screenshot, or diagnostic output. If the variable is missing,
tell the user only its name and that a scoped MCP credential is required; do not
ask them to reveal the value.

The local API and the Codex host must both be running before tools can appear.
After changing the environment or plugin, restart the host and use a new chat so
the tool catalog is discovered again.

## ChatGPT web

ChatGPT web cannot use the local loopback endpoint or a custom bearer/API key.
It requires a deployed HTTPS MCP resource plus the complete OAuth 2.1 MCP
authorization flow. Until DayWeave publishes protected-resource metadata,
authorization-server metadata, authorization-code plus PKCE, client
registration, consent, resource/audience validation, and OAuth token lifecycle,
state plainly that ChatGPT account linking is not available. Do not suggest
pasting a DayWeave token as a workaround.

Official references:

- <https://developers.openai.com/plugins/build/auth>
- <https://learn.chatgpt.com/docs/extend/mcp>
