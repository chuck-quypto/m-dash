# Licenses and notice generation

Application code/docs use the root [Apache 2.0 license](../LICENSE) and
[NOTICE](../NOTICE). [BRAND-LICENSE.txt](../BRAND-LICENSE.txt) separately covers
CND artwork and Hybrid Display. Companion fonts keep their SIL Open Font Licenses
under `src/brand/fonts/`; their texts are also included in generated notices.

Run `node scripts/generate-notices.mjs` on the Windows packaging host after installing
locked dependencies. It generates THIRD-PARTY-NOTICES.txt and licenses/inventory.json
from the locked target graph, including build dependencies conservatively, the
Tauri frontend API, fonts, and supplemental NSIS/WebView2/Rust notices.
Windows notice inputs use LF line endings through `.gitattributes`, so their
byte fingerprints match clean GitHub runners and local checkouts. Keep upstream
license wording and other whitespace unchanged.

On the Mac packaging host run
`node scripts/generate-notices.mjs aarch64-apple-darwin`. Outputs go under
licenses/macos/. The generator pins Mac Rust notice inputs to 1.94.1; refresh
those texts and its assertion if building with another compiler. Do not weaken
the check or package a Mac build with Windows notices.

On the Ubuntu x86_64 packaging host run
`node scripts/generate-notices.mjs x86_64-unknown-linux-gnu`. Outputs go under
`licenses/linux/`. Linux uses Rust 1.94.0's installed standard-library copyright
text, and records the actual installed Ubuntu versions and unmodified copyright
files for the dynamically linked GTK/WebKitGTK, Secret Service and tray components
and Zenity. These system packages are dependencies, not bundled libraries.
Refresh the pinned compiler notice before changing compilers. Generation does
not overwrite Windows or Mac inventories. A different Ubuntu host can produce
different system-component notices; regenerate on the actual packaging host.

Inventories record source URLs, versions, lockfile hash and notice hashes.
Each source URL supplies the exact published crate without charge. Preserve
source availability for unmodified MPL-2.0 components and include their .crate
archives and a source/hash manifest with release materials. Apache 2.0 does not
replace upstream terms. The public application source is separate from this
optional third-party source archive.

The shared `vendor/glib` copy carries the upstream iterator security fix for the
GTK3-compatible 0.18 API. Its `NOTICE.m-dash.txt` records the original archive,
the exact upstream fix and the patched source location. Linux notices include
that notice alongside the original MIT license and copyright.

Supplemental notices cover texts omitted from crate archives and platform/build
components. Keep upstream text/whitespace unchanged. Review inputs when toolchain
or components change. Updating one platform inventory does not qualify the other.

Binary packages must include LICENSE, NOTICE, BRAND-LICENSE.txt and matching
THIRD-PARTY-NOTICES.txt. Check actual package contents and deterministic notice
regeneration on each build host before publication.

