# Apple Silicon rc.3 parity acceptance

Private local development qualification on October 20, 2026. Installed checks and
the owner walkthrough passed on this Apple Silicon Mac. The owner confirmed that
all described tests worked. Evidence limits and remaining scope are recorded below;
this is not an official Mac RC or distribution approval.

## Exact final candidate

- Package source: `e7214f9e74080852a15fceebdaf5e9fb0f6bd69c`, based on main
  `c10617c8d38cded8871a36e05409645e9af6b3ad`, including Ubuntu merge `a1f17cb`.
  Branch: `codex/mac-rc3-parity`. Later workflow/report commits do not change the
  installed package's source identity.
- Source version: `0.4.0-rc.3`. A private build-config overlay sets bundle version
  `0.4.0-dev.mac.2`; identifier remains `net.chuckdudley.mdash`.
- Host: Apple Silicon, macOS 26.6.2 (25G83), Xcode 27.1 (27A9269), macOS SDK 27.0,
  Node 24.21.0/npm 11.8.0, Rust/Cargo 1.94.1. The configured macOS 12 minimum is
  not acceptance on that OS.
- Installed location: `/Applications/m-dash.app`.
- Signed executable SHA-256:
  `6ae3a0a39943efeb73b4df2b24aae8e6ccb93036abd31db2f7ed6cfa7fd32507`.
- DMG: `m-dash_0.4.0-dev.mac.2_macos-arm64.dmg`, 9,380,553 bytes; SHA-256:
  `0b591e17052a06ef06164f6d0e4dc0a90427d10bd08b88d5b4a79a36aebc54e1`.
- Both app and DMG have valid local Developer ID signatures; the app uses hardened
  runtime. Strict native installed-app verification passes. No notarization was
  submitted and no ticket is stapled. Gatekeeper rejects the app with
  `source=Unnotarized Developer ID`. This payload is not a trusted public download.

Private evidence and rollback materials remain under ignored
`.local/acceptance/mac-rc3-20261020/`. The final `package-2/` holds the DMG, explicit
license/user/privacy materials, matching Mac inventory/notices, five checksum-verified
MPL source archives, BUILDINFO with every app-file hash, and SHA256SUMS covering
16 files. No application source, account database or signing material is in that
package. Raw diagnostics and account captures stay local and uncommitted.

Tauri built and signed development build 1, then its Finder DMG-layout step failed.
The existing `scripts/dmg-settings.py` and already installed `dmgbuild` tool wrapped
that app with the approved artwork. The corrected app was then built app-only and
wrapped the same way, without rebuilding/re-signing its app during DMG generation.
The DMG was signed locally, mounted read-only and installed through Finder's
Applications replacement confirmation. Every installed app-file hash matched the
mounted final DMG before launch. No quarantine or Gatekeeper policy was changed.

## Verified gap and source checks

The first installed build reproduced a GUI Claude failure: the shared CLI search
selected an old `/usr/local/bin/claude` npm wrapper before the native installation.
That wrapper exited 127 because Finder's PATH supplied no Node runtime. The native
CLI returned a read-only `get_usage` response in the same minimal environment.

A disposable subprocess regression failed before the fix, then passed after Mac
selection preferred `~/.local/bin/claude`; it also checks fallback when that native
file is absent. Windows and Ubuntu retain their previous search order. All callers
use the same public discovery function. No dependency was upgraded.

- 72 desktop tests passed serially; three native opt-in tests were excluded from
  the ordinary suite. The dedicated nonsecret Keychain round-trip test passed
  separately using its isolated test service. No blanket ignored suite ran.
- Five shared-sync tests, strict desktop Clippy, desktop/shared formatting,
  TypeScript/Vite build and release-doc checks passed locally.
- Seven Windows/Mac RC tests passed, including native synthetic DMG/app mismatch,
  corrupt MPL source, explicit payload/checksum and disabled-workflow checks.
- Existing synthetic browser checks passed startup/read/version/refresh recovery,
  healthy native-bridge classification, ordinary-browser warning, dropdown focus,
  pointer interaction during refresh, separate Claude errors and strip DST updates.
- Mac notices regenerated identically for the actual `aarch64-apple-darwin` compiler.
  Other-platform inventories, all dependency locks, Ubuntu code and approved CND
  assets/native icons/DMG background were preserved.
- Initial socket tests failed under sandbox restrictions, then passed with local
  loopback access. Linux packaging checks require Linux components/tools and were
  not qualified on this Mac. Native Windows/Ubuntu retesting was not performed.

## Installed results

| Check | Final development package evidence |
| --- | --- |
| Finder launch / startup | Correct Applications executable/version; no browser-only startup warning. |
| Four providers / GUI discovery | Fresh Codex, Claude, Grok and OpenRouter readings after Finder launch. Native CLIs: Codex 0.162.1, Claude 2.1.301, Grok 1.0.62. No model request was used for these reads. |
| Keychain / native prompt | Retained production key read successfully after replacement/restarts; masked native replacement prompt opened and Cancel preserved the connection. Live key replacement/delete was not exercised. |
| OpenRouter browser action | Native fixed management-key link was invoked successfully; no key was entered or transmitted by automation. |
| History / settings | Native Codex, Claude-local and OpenRouter-imported tables displayed retained rows. Provider pause survived restart; re-enable recovered Grok. Four manual compact choices survived a full quit/relaunch; original automatic selection and all four provider choices were restored afterward. |
| Branding / strip | Approved Hybrid dashboard, heading, native tile and DMG artwork; complete strip wordmark/monogram at 528 by 88. Minimize showed four selected bars; drag and strip restoration were exercised. Full-screen layout and return passed on the connected display. |
| Single instance / close | Close retained the installed process/receiver. A second executable launch exited successfully, kept the same single PID and visibly restored the dashboard. |
| Menu bar / Dock | Owner reports direct menu-bar Open/Refresh/Quit and Dock-click reopen passed. These direct click paths were checked by the owner because the computer-use tool could not access the Dock/menu-bar host. |
| Quit / cleanup | Native Command-Q fully exited; receiver port became available and collector children were gone. Owner reports direct menu-bar Quit passed; no installed app process was running at the post-walkthrough check. |
| Offline / sleep recovery | Owner reports the described disconnect/refresh/reconnect recovery and physical sleep/wake checks passed. These are owner-observed native results on this Mac. |
| Idle CPU | Existing native script passed on the final package: 0.00 CPU seconds in about 3 seconds while minimized, and 0.01 in 3.01 seconds on the final dashboard. These are short idle samples, not long-duration power qualification. |
| Claude feed recovery | Applications helper path, native masked/config behavior, disable/re-enable, fresh token per app start and retired-token HTTP 404 rejection passed. Exact installed helper delivered only selected synthetic accounting metadata to an isolated receiver while the real app was stopped; synthetic events did not enter owner history. Occupied-port and cleanup/recovery cases passed source tests. Owner reports the described Claude restart/feed check worked; the automated postcheck found no new persisted token event, so fresh ordinary token delivery is not independently corroborated. |
| Pings | Found enabled in the old installation and turned off through Settings before checks. Stayed off across upgrades/restarts; historical attempt records are unchanged. No new ping was requested. |

## Data-preservation evidence

The previous installed `0.4.0-rc.2` app passed native signature and notarized
Gatekeeper checks before replacement. Its fully quit bundle and complete data
folder, including SQLite working files, were copied to a private rollback location.
The rollback executable matched its original bytes and the backup database passed
`PRAGMA integrity_check`. External Claude settings were copied with resolved path,
mode and symlink status recorded; Keychain entries were not exported.

Every data-file hash and the external settings hash stayed unchanged through the
first Finder replacement before launch. A second consistent snapshot was taken
before the corrected replacement, whose data-file hashes also stayed unchanged
before launch. Expected collection and feed-token rotation began only on launch.

After multiple restarts, all 7,626 original observation rows remained exact, both
original Claude request rows remained exact, all 190 daily-history keys remained,
and all original session, provider-account and latest-snapshot keys remained.
New read-only observations/session metadata were allowed. Ping records were exact
and off. Unrelated Claude settings were identical outside the rotated endpoint.
Final provider/compact preferences matched the original backup exactly. Full
row/hash comparisons and account-specific details remain in private evidence.

## Owner walkthrough and remaining scope

The owner reported that all described tests worked: direct menu-bar controls and
Dock reopen, physical offline/reconnect recovery, sleep/wake, and the Claude
restart/feed check. Those results are recorded as owner-observed acceptance,
separately from the automated native checks above.

The post-walkthrough read-only check verified every installed app-file hash and
the final DMG hash, database integrity, all original history rows/keys, exact
provider/compact preferences, and ping-off state with unchanged attempt records.
The repository remained private and the hosted build/attestation gates remained
disabled. The app was quit at that check and was left as the owner left it.

There were still only the two original persisted Claude token-request records.
The owner's feed check does not independently establish a fresh numeric token
event in the retained database. Observe one during ordinary future Claude use;
no new inference or ping was triggered for acceptance.

- Extra monitor/scaled-display transitions, clean user/second Mac, older macOS and
  Intel/universal qualification remain unrun. Live Keychain replacement requires
  owner-controlled secret entry if it is desired.
- Installed acceptance of the signed, notarized DMG from the
  [Release workflow](../../RELEASING.md) is still required. That workflow has not
  been dispatched.

No visibility change, publication, service deployment, public attestation,
transparency-log entry, signing-credential creation/export/upload or Apple
notarization submission occurred. Android and relay work were excluded.
