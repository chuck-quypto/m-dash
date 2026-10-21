# Application structure

The desktop app uses Tauri 2: TypeScript renders the local dashboard and Rust owns
collection, persistence and credentials. Vite produces bundled local assets;
provider secrets do not belong in the renderer. No remote fonts are loaded.

- `src/main.ts` renders dashboard/settings and calls native commands.
  `src/compact.ts` renders the strip. `src/brand/` holds vendored assets.
- `src-tauri/src/lib.rs` wires commands, scheduling and lifecycle. Provider modules
  normalize CLI/API responses; `dashboard.rs`, `compact.rs` and `forecast.rs`
  derive visible state without inventing missing capacity.
- `store.rs` keeps SQLite observations, account-separated history and settings.
  `credentials.rs` uses platform storage for OpenRouter. Provider CLIs retain
  subscription sign-in; collection does not extract their login secrets.
- Claude's optional accounting feed uses an authenticated loopback receiver;
  `claude_setup.rs` manages reversible integration. `claude_ping.rs` and its runner
  implement separately consented, off-by-default model requests with durable
  attempt tracking. Collection and inference are separate paths.
- `mobile_sync.rs`, `sync-core/`, `relay/` and `mobile/` implement experimental
  encrypted companion delivery. They are outside desktop release scope but remain
  security-relevant code. See [the protocol](development/SYNC_PROTOCOL.md).

Errors retain last-good readings with their original time. Disabled providers stop
collection without destroying saved state. Native tests and synthetic browser
fixtures cover different boundaries; installed lifecycle and credential behavior
require platform qualification.

See [development](DEVELOPMENT.md), [providers](PROVIDERS.md), [privacy/storage](PRIVACY.md),
and [releases](../RELEASING.md) for operational details.
