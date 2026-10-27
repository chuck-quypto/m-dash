# Installing and using m-dash

m-dash is a local Windows and Mac dashboard for AI allowances, resets, credits and usage. You can use any subset of Codex, Claude, Grok and OpenRouter. Account collection reads information without sending model prompts or purchasing credits. A separate Claude ping feature is off by default and makes a model request only after you enable it through its usage confirmation.

## Windows installation

The Windows target is x64. Download the setup executable from the [Releases page](https://github.com/chuck-quypto/m-dash/releases) and [verify it](../RELEASING.md#verify-a-download). The installer is not Authenticode-signed, so SmartScreen may warn the first time you run it. Do not disable Windows security to install it.

Open the setup executable from **File Explorer**. The default per-user location is `%LOCALAPPDATA%\m-dash`, with a Start-menu shortcut. The release package includes this guide, PRIVACY.md, LICENSE, NOTICE, BRAND-LICENSE.txt and THIRD-PARTY-NOTICES.txt alongside the app. You do not need Node, Rust, Visual Studio or a database server to run m-dash itself.

Microsoft WebView2 is required. The installer uses an existing runtime or downloads it when missing; that download requires internet access. WebView2 is shared with other applications. Launching setup from a packaged development host can redirect files into that host's private cache, so use Explorer.

## Mac installation

Use the Apple Silicon DMG on an Apple Silicon Mac. Open it in Finder, copy m-dash to Applications, then open it from Applications. macOS supplies WebKit; Node, Rust, PowerShell and WebView2 are not needed. The DMG is signed with a Developer ID and notarized by Apple. Intel/universal builds are not provided. Do not bypass Gatekeeper.

For an upgrade, choose **Quit m-dash**, then replace the app in Applications. Keep `~/Library/Application Support/net.chuckdudley.mdash` to retain settings and history. OpenRouter keys are stored separately in your login Keychain. The optional Claude feed uses the installed m-dash executable; keep the app in Applications after enabling the feed.

Mac CLI discovery includes PATH and standard Homebrew directories for Codex, `~/.local/bin` for Claude and `~/.grok/bin` for Grok. Claude prefers its native `~/.local/bin` installation before PATH or legacy npm wrappers, which may require a Node runtime unavailable to Finder. Install and sign in to each provider CLI you enable. Authentication remains subscription-based and owned by those CLIs.

## Ubuntu development installation

Ubuntu x86_64 builds work for development but are not yet a supported release,
and no Linux download is published. Build the `.deb` yourself as described in
[development](DEVELOPMENT.md#ubuntu-development-package).
Quit m-dash before reinstalling/upgrading and retain
`${XDG_DATA_HOME:-$HOME/.local/share}/net.chuckdudley.mdash` for settings/history.
The package uses Ubuntu's GTK/WebKitGTK runtime and canonical M- launcher icon.
Launch from Applications; provider discovery includes `~/.local/bin` without
executing shell startup files, plus Claude's native and Grok's standard locations.

OpenRouter **Connect key** opens a native masked Zenity dialog. Validation precedes
storage in your desktop Secret Service keyring. The key stays out of the renderer,
process arguments and environment. Unlock the keyring yourself if needed. A locked
or unavailable keyring reports an error and retains the existing connection; there
is no plaintext fallback. Cancel leaves the existing connection unchanged.

The tray needs an active StatusNotifier/AppIndicator host (Ubuntu's AppIndicators
extension provides one). Use its **Open m-dash**, **Refresh accounts**, and
**Quit m-dash** menu items. Minimize for the compact strip and click its wordmark
to restore. If no reachable tray is available, closing quits instead of hiding
collection. A second launch restores the existing instance. Startup at sign-in
is not enabled. On Linux, startup prefers X11/XWayland when `DISPLAY` is available
and `GDK_BACKEND` is unset. Native Wayland did not show the minimized strip on
this GNOME host and cannot provide its placement/stacking contract. An explicit
`GDK_BACKEND` choice is honored; native Wayland strip parity and a separate Xorg
login are not qualified. No desktop session or shell extension is changed.

The optional Claude feed uses the installed executable on Ubuntu, preserving
custom settings, settings-file permissions and symlinks. Its token-bearing bridge
is private to your user. Restart Claude Code after restarting m-dash. Automatic
Claude pings remain off unless you explicitly review and enable them. Mobile
publishing is unavailable on Linux; no relay or mobile vault is configured.

## Choose your accounts

On first launch, select providers in **Settings** and save. Each has its own prerequisites:

| Provider | Connection |
|---|---|
| Codex | Install Codex CLI and sign in with your subscription. Its native executable must be discoverable on PATH or through the usual Windows npm installation. |
| Claude | Install the native Claude Code CLI and sign in with your Claude subscription. m-dash checks PATH and your `.local/bin` directory. |
| Grok | Install Grok CLI and run `grok login`. m-dash checks PATH and your `.grok/bin` directory. |
| OpenRouter | Enable the provider, open **OpenRouter management keys**, create a management key, and use **Connect key**. Enter the key in the native secure prompt (Password on Windows; management-key field on Mac). |

Plan names and windows come from the provider. Codex five-hour and weekly windows are supported without selecting Plus or Pro manually. A missing measurement is unavailable, never zero. Claude's account-usage reader uses an experimental native CLI contract and may need an m-dash update when Claude changes it.

The optional Claude local feed records token totals from this device while m-dash is running. Enable it in Settings and restart Claude Code. Restart Claude Code again after restarting m-dash: the local receiver uses a fresh token each time and updates its managed exporter endpoint. Existing custom statuslines/telemetry are preserved and may need manual composition. **Disable local feed** removes only the configuration m-dash added. Turning off the Claude provider pauses recording but leaves that feed configuration in place.

Both Windows and Mac use the installed m-dash executable as the feed helper. Windows uses a fixed PowerShell command to launch it through either of Claude's supported shells; it does not run a `.ps1` file or change execution policy. Startup updates an existing managed helper path when the app moves, preserving custom settings and settings-file symlinks. On Mac, finish copying the app to Applications before enabling the feed.

Idle Claude allowances with no reset time are omitted until Claude reports an active window. This does not invalidate the other allowances. The most recent native CLI account read selects the Claude account; local telemetry for another account retains separate history. A transient ping preflight failure pauses that check without removing your saved consent or sending a model request. A confirmed account or ping-model change still requires new consent.

OpenRouter imports only the completed UTC days actually returned by its activity API. Missing days retain their previous totals; an omitted day is not assumed to have zero usage.

If its local receiver cannot start or stops unexpectedly, m-dash suspends the feed for safety and removes its managed delivery settings. History and unrelated Claude settings are preserved. Quit Claude Code, restart m-dash, enable the local feed again, then reopen Claude Code. A suspended feed stays off when the listener recovers. Already-running Claude processes can retain their old exporter environment, so changing settings alone does not stop those processes from sending telemetry. If cleanup reports an error, keep Claude Code closed until the settings problem is repaired. Account allowance reads are separate from this optional token feed.

Turning a provider off pauses its reads and hides it without deleting saved history or credentials. A read already in flight may finish, but its result is discarded. To remove an OpenRouter key, enable its provider and choose **Disconnect** before turning it off again. Management keys have broader authority than m-dash needs; the app only reads credits/activity.

## Tray, compact display and updates

Settings can choose compact allowance windows automatically or display up to four selected windows. Automatic selection prioritizes one window per enabled allowance provider, then weekly windows. Selected bars always appear alphabetically by provider, with five-hour before weekly within each provider, regardless of the order they were enabled. This order is shared by the tray icon and compact strip on Windows and Mac. OpenRouter credits remain in the full dashboard. A manually selected window stays unavailable if the provider does not supply it.

Minimize for the draggable compact strip; click **m-dash** in the strip or the tray icon to reopen. Closing the window keeps collection running. Use **Quit m-dash** in the tray to exit. On Mac, the tray lives in the menu bar. Windows may put the tray icon in its hidden-icons menu. Startup at sign-in is not enabled.

Readings marked saved or stale are last-known values. Collection needs a running computer and internet access; forecasts stop when the evidence is stale or insufficient.

For a Windows manual upgrade, choose **Quit m-dash**, then run the newer installer from Explorer. When it finds the installed version, choose **Do not uninstall**: the new version installs over the old one and keeps your settings and history. If you choose **Uninstall before installing** instead, leave **Delete the application data** unchecked in the uninstaller. That option permanently deletes `%LOCALAPPDATA%\net.chuckdudley.mdash`, which holds every setting, provider choice, Claude ping consent and all history; it is not moved to the Recycle Bin. Automatic updating is not included yet. See PRIVACY.md for storage details.

## Optional Claude ping

In **Settings → Automatic window start**, choose **Off · enable…** for Claude ping. Read the confirmation and choose **Enable automatic pings**, or **Cancel** to leave it off. This control applies immediately and separately from the general Save settings button. It requires the native Claude Code CLI 2.1.293 or later, signed into your own Pro or Max subscription. Codex is pending account testing and cannot be enabled.

When a fresh, successful allowance read identifies an idle five-hour window and available weekly allowance, m-dash makes one recorded attempt to send `ping` to Haiku 5.5 at low effort. It consumes subscription/weekly usage and may consume paid credits under Claude's extra-usage settings. Harness overhead means exact token cost is not fixed. It starts a window; it does not immediately refill an allowance or guarantee capacity later.

Keep m-dash running and enable this on only one computer per account. The app checks with its regular Claude refresh, so it need not run at the exact expiry second. Unknown/stale data, missing weekly allowance or a CLI/authentication problem prevents sending. Failed or uncertain attempts are not automatically replayed, including across restart and off/on toggles. A new observed five-hour window permits a later attempt, at least five hours after the previous one. The status shows the last attempt and reported tokens when available. Claude Code may apply its own managed settings and transport behavior.

Choose **On · turn off** to stop future pings. An already submitted request may finish. Turning off the Claude provider also disables ping. A new sign-in needs fresh confirmation. The local token feed is independent; turning that feed off does not turn ping off. Your installation starts with ping off, including after upgrade.

## Android companion (experimental)

Android and mobile sync are experimental and outside this desktop release. A fresh install has no configured relay or enrollment credential. No managed relay or Android download is promised. Developers can review [relay setup boundaries](../relay/README.md) and the [protocol](development/SYNC_PROTOCOL.md).

After a relay is separately configured, **Settings → Mobile companion** can create a ten-minute pairing code following its retention notice. Transfer it privately to the Android app. Compare verification values on both devices and approve only a match. The desktop uploads within one minute; Android Refresh retrieves saved readings.

Keep the publishing computer and m-dash running for fresh uploads. Windows uses Windows Credential Manager; Mac uses Keychain. Each computer has its own pairing and readings; changing the phone's source requires disconnecting and pairing again. Android only displays saved usage readings; provider credentials and pings remain on the desktop. Older readings stay labeled stale and never invent a reset. Browser previews support sample data only. iPhone, background phone refresh and widgets are not enabled in this development build.

Use **Revoke & remove** on the publishing desktop to stop relay delivery. If offline, keep m-dash running so removal can retry. Use **Disconnect & clear this phone** on Android to remove its protected local copy. A storage failure offers retry or explicit removal; removing it requires a new pairing. Read PRIVACY.md for the seven-day active snapshot expiry, possible 30 additional days in Cloudflare recovery history, and metadata disclosure.
