# CND brand assets

Vendored from the maintainer's CND brand source (not published), revision of
October 14, 2026.

Generated token CSS, product font CSS/fonts, source tokens and the unchanged dark
monogram are copied from that revision. Update them together.
Fonts remain under their accompanying SIL Open Font Licenses. No remote font
requests or private Git dependency are needed at build or runtime.

m-dash uses the dark/product/quiet palette. The experimental Hybrid identity adds
the unmodified `logo/cnd-monogram-hybrid-transparent.png` and
`dist/hybrid/cnd-hybrid-display-v1.woff2` from the brand source revision of
October 18, 2026.
The monogram uses the brand's documented `invert(1) hue-rotate(180deg)` dark
treatment in the upper-right portfolio signature. Hybrid is reserved for the
upper-left M- app icon, product name (including the compact strip), and main heading; body,
controls, and numbers retain the original companion fonts. ASCII lowercase in
Hybrid maps to capitals. The vendored Hybrid font and CND artwork have separate
permission in [BRAND-LICENSE.txt](../../BRAND-LICENSE.txt), included with source and
binary packages. The SIL licenses above apply only to the companion fonts.

Run `npm run dev` and open `/preview/` for a synthetic-data preview. It uses the
real UI with an isolated mock bridge; it does not contact providers or save
account settings. No native rebuild or installation is needed to review it.

The packaged desktop icon and Mac installer also use Hybrid. Desktop icon
outlines reuse `mobile/app-icon.svg` on the desktop tile; `app-icon.svg` is the
regeneration source. The installer renderer outlines the bundled static Hybrid
font and embeds the monogram using the same dark treatment.
