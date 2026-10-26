# Changelog

Notable changes to the m-dash desktop app. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added
- Allowance bars now warn about pace, not only what is left. A bar turns amber
  when the window's average pace would use it up before the reset, and red when
  it would run out well before it. Under 20% and under 5% left still turn amber
  and red.
- A ⚡ marker on an otherwise healthy bar when recent use runs at least twice the
  window's average and would empty it before the reset.
- Every amber, red and ⚡ bar also shows a symbol (?, ! or ⚡) and says why in
  words, so the warnings don't depend on telling colors apart. The dashboard's
  Codex bars now use the same colors as the strip and tray.

### Security
- Patched GLib's iterator soundness issue (RUSTSEC-2024-0429) in the Linux build.

## [0.5.0] - 2026-10-22

First public release. Targets Windows x64 and Apple Silicon macOS.

### Added
- Choose Codex, Claude, Grok and OpenRouter independently, retaining history when
  disabling a provider.
- See reported allowances, resets, credits and numeric usage history, with explicit
  stale/unavailable states and conditional capacity estimates.
- Monitor up to four allowance windows in the compact strip and tray/menu bar.
- Store OpenRouter credentials in Windows Credential Manager, macOS Keychain or the
  Linux Secret Service.
- Optionally collect local Claude token history through Claude Code's statusline
  and telemetry exporter.
- Optional Claude ping, off by default, which requires separate consent for its
  model request and account usage.
- CND Hybrid identity, display font and monogram in the dashboard and strip.

### Changed
- Long-running installs stay small: raw allowance readings and idle Claude
  sessions are removed after 30 days, and Claude request identifiers older than
  35 days are folded into daily totals.

### Notes
- Windows installers are not Authenticode-signed. Releases include SHA-256
  checksums and GitHub build attestations instead.
- No automatic updater or login startup. Quit from the tray/menu bar before
  installing a newer version; keep the data directory to retain history.
- Android, encrypted mobile sync and the relay are experimental and are not part
  of this release.

[Unreleased]: https://github.com/chuck-quypto/m-dash/compare/v0.5.0...HEAD
[0.5.0]: https://github.com/chuck-quypto/m-dash/releases/tag/v0.5.0
