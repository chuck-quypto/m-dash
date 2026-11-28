# m-dash

Local desktop dashboard (Rust + Tauri 2 + TypeScript/Vite) showing allowances, resets, credits, and usage history for Codex, Claude, Antigravity, Grok, and OpenRouter. Public repo (Apache-2.0, DCO required); Windows x64 and macOS Apple Silicon are release targets.

## Verification

Run the applicable CI checks before opening a PR (Node 24+, stable Rust):

```sh
npm run build
cargo test --manifest-path src-tauri/Cargo.toml --locked --lib -- --test-threads=1
cargo test --manifest-path sync-core/Cargo.toml --locked
cargo clippy --manifest-path src-tauri/Cargo.toml --locked --all-targets -- -D warnings
cargo fmt --manifest-path src-tauri/Cargo.toml --check
node scripts/check-release-docs.mjs
```

Mobile/sync tests (when touching `mobile/`): `npm run mobile:build && npm run mobile:test`.

## Context pointers

- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) — Tauri backend, collectors, storage, and frontend layout.
- [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md) — Linux builds, synthetic previews, packaging, and notices.
- [docs/PROVIDERS.md](docs/PROVIDERS.md) — provider CLI contracts and measurement boundaries.
- [docs/PRIVACY.md](docs/PRIVACY.md) — storage, redaction, and consent invariants; read before changing persistence or logging.
- [RELEASING.md](RELEASING.md) & [licenses/README.md](licenses/README.md) — release steps and regenerating `THIRD-PARTY-NOTICES.txt` after dependency changes.

## Rules

- **No credential extraction:** Provider CLIs own authentication; m-dash never reads or extracts their login secrets. OpenRouter keys live in OS credential storage.
- **Consent & zero surprise spend:** Collection never sends model prompts. Optional Claude `ping` stays off by default behind explicit consent.
- **Synthetic tests only:** Tests never contact live provider accounts/relays or make paid inference calls. Keep real captures, databases, pairing codes, and signing keys out of git.
- **Data durability:** Preserve user history and account separation across failures and upgrades; never weaken freshness or storage checks to make a test pass.
- **DCO & brand terms:** Sign every commit with `git commit -s`. CND assets follow [BRAND-LICENSE.txt](BRAND-LICENSE.txt).
