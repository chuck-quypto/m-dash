# Android companion preview

Experimental Android companion, outside the Windows/Mac release scope. Encrypted desktop-to-phone sync is implemented for development. Source tests and limited installed checks do not establish public Android readiness. Provider credentials, desktop commands and model calls are never supported on the phone.

## Run and test

From the repository root, using Node 24+:

```sh
npm ci
npm run mobile:test
npm run mobile:dev
npm run mobile:build
```

Open the printed local URL. A fresh install is disconnected. Choose **Explore sample data** to opt in, then try the available/stale/offline/unavailable scenarios. **Exit preview & clear** removes sample storage. Reload restores labeled sample data only. This browser workflow does not establish native Android acceptance.

The native sample-cache Rust test uses Unix directory sync and currently fails with `Sample storage unavailable` on a Windows host. Run that test on macOS/Android; the Windows frontend tests, native Clippy check and Android cross-build remain separate checks. Do not remove Android's directory sync to make the Windows host test pass.

## Build Android

Install JDK 17 or 21, the Android command-line tools, SDK platform 36, build tools 36.0.0, NDK 28.2.13676358 and Rust target `aarch64-linux-android`. `npm run android` works in PowerShell and on macOS/Linux. Its Node launcher defaults to `.local/android-sdk` and that SDK's pinned NDK without changing shell configuration. Set `JAVA_HOME` to your JDK; the existing Homebrew JDK 17 default is retained on Mac. Existing `ANDROID_HOME` and `NDK_HOME` overrides are respected.

On Windows, enable **Developer Mode** in Windows Settings before building; Tauri needs it to create symbolic links for the native Android library. The SDK command-line tools are sufficient; Android Studio and an emulator are optional when installing on a physical phone.

```sh
rustup target add aarch64-linux-android
npm run android -- build --debug --target aarch64 --apk
```

The native Gradle project under `mobile/src-tauri/gen/android` is committed. Do not regenerate it routinely: it pins API 36 (the CLI template initially selected unavailable API 37), disables Android backup and contains the m-dash launcher icons. Build output is ignored. The mobile package has its own Tauri command so Gradle cannot accidentally invoke the desktop project.

The Hybrid launcher source is `mobile/app-icon.svg`, using the CND Hybrid M and
hyphen outlines inset to fit Android's circular mask. Regenerate with
`node node_modules/@tauri-apps/cli/tauri.js icon mobile/app-icon.svg --output
.local/android-hybrid/generated`, then copy only the
generated `android/mipmap-*/*.png` files to the matching Android `res` folders.
Keep the existing adaptive-icon XML and dark background color. The mobile header
shares the desktop Hybrid font and brush monogram; numbers and controls retain
the readable companion fonts.

Expected artifact: `mobile/src-tauri/gen/android/app/build/outputs/apk/universal/debug/app-universal-debug.apk`. This is a debug-signed ARM64 development APK, not a release-signed/store-ready artifact. Do not put signing keys into Git. Native cache/TLS, foreground refresh, retained pairing, offline handling and revocation need separate device checks; this build command does not qualify them.

## Sideload from Windows

Connect an ARM64 phone running Android 8 or later by USB, enable **Developer options → USB debugging**, unlock it, and approve the computer's debugging prompt. From the repository root with the default local SDK:

```powershell
& .local/android-sdk/platform-tools/adb.exe devices -l
& .local/android-sdk/platform-tools/adb.exe install -r mobile/src-tauri/gen/android/app/build/outputs/apk/universal/debug/app-universal-debug.apk
& .local/android-sdk/platform-tools/adb.exe shell am start -n net.chuckdudley.mdash.mobile/.MainActivity
```

`install -r` preserves existing app data when the installed package has the same signing key. A debug APK built on another computer usually uses a different debug key; Android will reject that upgrade. Preserve/export needed state and decide how to handle the old install before uninstalling it. Do not clear phone data to work around a signing mismatch. If Windows sees the phone but ADB does not, check USB debugging and the manufacturer's ADB driver. A Pixel can use Google's USB driver from SDK Manager.

## Boundaries

`src/snapshot.ts` admits bounded normalized snapshots; `src/companion.ts` owns refresh/cache/replay state; `src/sample.ts` supplies fictional readings; `src/view.ts` renders admitted data; `src/main.ts` binds UI events. The native shell contains no provider integration. Its bounded commands cover sample storage and live pairing/status/pull/clear. Live networking and authenticated envelope admission use the shared `sync-core` Rust crate; Android Keystore protects pairing and cache state in backup-excluded storage. Checked atomic persistence completes before success. CND tokens/fonts/monogram retain their bundled licenses.

`pullCloudflare()` remains the disconnected/browser fallback and makes no request. The installed Android app can accept a ten-minute code from desktop Settings → Mobile companion. Review the disclosure, compare verification values on both devices, approve on desktop and refresh on Android. The WebView CSP still permits local IPC only; native HTTPS requests are bounded and refuse redirects. Samples and live protected state never share a cache. Clear on Android removes its copy; revoke on desktop stops relay delivery.

The desktop publisher supports Windows Credential Manager and macOS Keychain. Each computer needs its own secure relay setup and phone approval. The development relay is restricted to Cloudflare `workers.dev` origins; self-hosting needs separate qualification. See [relay development boundaries](../relay/README.md) and [protocol](../docs/development/SYNC_PROTOCOL.md) before live data. No relay address or enrollment credential is bundled for public users.

Cellular away-from-home use, TalkBack, release signing, dependency notices, production sync/security, iPhone and publication remain separate gates.

For the native TLS regression, install the current debug APK on the unpaired disposable `emulator-5554`, then run `node scripts/check-android-tls.mjs` with Playwright available (or set `MDASH_PLAYWRIGHT_MODULE` to its module path). It must receive the real relay HTTP 401 without hanging; no enrollment or provider credential is used. Host tests do not exercise Android verifier initialization.
