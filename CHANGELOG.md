# Changelog

Notable changes to the m-dash desktop app. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Fixed
- The **API price** column on Allowance vs tokens shows "—" for usage whose
  source reports no price, such as terminal sessions read from session logs,
  and marks partly priced rows with "+". Before, those rows showed $0.00, which
  read as free.

## [0.5.4-rc.2] - 2026-11-08

### Fixed
- Claude Code cloud sessions now actually report their usage. Cloud environments
  drop variables named `OTEL_…`, so the four variables that **Cloud sessions and
  other computers** used to give were never seen by cloud sessions. Settings now
  gives a **setup script** to paste into the cloud environment instead; it writes
  the same settings into Claude Code's own settings before each session starts.
  The relay host still has to be in the environment's allowed domains.
- The **Observed usage** table on Allowance vs tokens always lists cloud-session
  rows. Before, it showed only the 12 largest groups, so cloud usage that had
  arrived could be hidden behind 28 days of local usage.

## [0.5.4-rc.1] - 2026-11-07

Release candidate for 0.5.4. Version 0.5.3 was prepared but never published, so
its changes are included here.

### Added
- A new **Allowance vs tokens** page compares how far each Codex and Claude
  allowance moved with the tokens seen on this computer over the last 28 days.
  It learns the account's own points-per-token rate from its most efficient
  days, shows the rest as unattributed usage (cloud sessions, chat and mobile
  apps, background agents, other computers), dates any sustained rate shift,
  and breaks observed usage down by where it ran, its purpose and its model.
  For Codex it also compares account-wide daily tokens with this computer's.
  Copy the daily figures as CSV.
- Optional **Local session logs** setting (off by default) reads the session
  logs Codex and Claude Code already keep on this computer, keeping only
  per-request accounting fields.
- The Claude local feed now keeps each request's model, token types, API-priced
  cost, purpose (main, subagent, compaction), effort, speed and whether it ran
  in a cloud session, and records requests refused for a reached limit.
  Allowance readings are kept for 400 days.
- Optional **Cloud sessions and other computers** setting shares per-request
  token counts between your own computers and receives usage from Claude Code
  cloud sessions through your own relay, so the Allowance vs tokens page can
  attribute usage this computer never sees. Requires a relay update.
- Each Codex and Claude allowance bar shows how long the allowance lasts at this
  window's average pace, such as "About 3 hours left at this pace", or "Lasts to
  reset at this pace". It uses the same pace as the amber and red alerts, so the
  two always agree, and appears once a tenth of the window has passed. After an
  idle stretch it lags behind; the ⚡ surge alert still covers a sudden burst.

### Changed
- The Allowance vs tokens rate-shift note now says how many days of data it rests
  on and that a plan change, a provider change or more usage elsewhere can cause
  it, instead of presenting the shift as a provider rate change.

### Fixed
- Claude allowance readings keep working with the newer usage format saved by
  recent Claude Code versions, which can leave a window's percentage empty or
  list per-model limits.
- The Claude local feed also recovers when an earlier version already started
  after its data folder was deleted. That start saved a new key while Claude kept
  sending token history with the old one, so the Daily history stayed empty and
  turning the feed off failed. 0.5.2 only reclaimed the feed when the folder was
  still missing; m-dash now reclaims its own entries whenever Claude holds an
  older key of the same shape, and issues a new one.
- Results of Settings actions, including errors, now appear beside the button
  that produced them. Before, they appeared at the top of the page, so an error
  from **Disable local feed** could be out of view and the click looked ignored.
- **Settings saved** and other Settings messages clear when you leave Settings,
  instead of staying on every page.
- The ! and ⚡ marks beside large Codex allowance values are readable again.
  They had picked up the muted color of the "% left" text.

## [0.5.2] - 2026-10-27

### Fixed
- The Claude local feed recovers after its data folder is deleted, for example by
  the Windows uninstaller's **Delete the application data** option. Before, Claude
  kept sending token history with the old key, so m-dash showed the feed as on but
  recorded nothing, and turning it off failed. m-dash now reclaims its own entries
  in Claude's settings at startup and issues a new key.

### Changed
- The user guide's Windows upgrade steps now say to choose **Do not uninstall**,
  and warn that **Delete the application data** permanently removes settings and
  history.

## [0.5.1] - 2026-10-26

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

[Unreleased]: https://github.com/chuck-quypto/m-dash/compare/v0.5.4-rc.2...HEAD
[0.5.4-rc.2]: https://github.com/chuck-quypto/m-dash/compare/v0.5.4-rc.1...v0.5.4-rc.2
[0.5.4-rc.1]: https://github.com/chuck-quypto/m-dash/compare/v0.5.2...v0.5.4-rc.1
[0.5.2]: https://github.com/chuck-quypto/m-dash/compare/v0.5.1...v0.5.2
[0.5.1]: https://github.com/chuck-quypto/m-dash/compare/v0.5.0...v0.5.1
[0.5.0]: https://github.com/chuck-quypto/m-dash/releases/tag/v0.5.0
