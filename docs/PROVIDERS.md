# How m-dash reads each provider

m-dash asks each provider's own CLI or API for usage. It never reads or copies
subscription login tokens. Several of these interfaces are not documented as
stable, so a provider update can break a reading without warning. When that
happens, m-dash keeps the last good values, labels them stale, and shows the error.

If a reading breaks after a provider update, open a **Provider breakage** issue
with the CLI version (`codex --version`, `claude --version`, `grok --version`) and
a redacted description of the error.

| Provider | What m-dash runs | Interface stability | What tends to break |
| --- | --- | --- | --- |
| Codex | `codex app-server` over stdio JSON-RPC: `account/read`, `account/rateLimits/read`, `account/usage/read` | App-server protocol used by Codex integrations | Renamed methods or fields; signing in with an API key instead of a ChatGPT subscription |
| Claude | Native Claude Code CLI in `--print` stream-JSON mode with a `get_usage` control request, then Claude's saved usage in `~/.claude.json` | **Experimental.** Internal control request and cache format | New or renamed allowance windows; cache format changes; npm wrapper installs instead of the native CLI |
| Claude (optional local feed) | Claude Code statusline command and OpenTelemetry log exporter, pointed at m-dash's loopback receiver | Documented Claude Code settings | Custom statuslines need manual composition; Claude Code must restart after m-dash restarts |
| Grok | `grok agent --no-leader stdio`: `_x.ai/auth/info` and `_x.ai/billing` | Vendor extension methods | Method or field changes; token history is not available |
| OpenRouter | HTTPS `GET /api/v1/credits` and `/api/v1/activity` with your management key | Public API | The current UTC day is not reported; management keys must stay valid |

## What m-dash never does

- Send prompts during collection. The only model request is the optional,
  off-by-default [Claude ping](USER_GUIDE.md#optional-claude-ping).
- Buy credits or change provider settings, apart from the Claude Code statusline
  and telemetry entries that the optional local feed adds and removes.
- Fall back to an API key when a subscription CLI is signed out.

See [privacy and storage](PRIVACY.md) for what is stored locally.
