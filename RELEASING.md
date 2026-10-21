# Releases and verification

## How releases are built

- Every release is built from a reviewed commit on `main` that is tagged
  `v<version>`. Versions are synchronized across `package.json`, the Cargo
  manifests and lockfiles, and `tauri.conf.json`.
- Builds use locked dependencies and run the full test, Clippy, formatting and
  license-notice checks before packaging.
- Both installers are built on GitHub-hosted runners by the manually dispatched
  [Release workflow](.github/workflows/release.yml). Its preflight rejects a
  commit or version that doesn't match the reviewed request.
- macOS builds are signed in that workflow with the maintainer's Apple Developer
  ID. The app and the DMG are notarized by Apple and stapled.
- Each release ships its installers with `SHA256SUMS`, the license notices, a build
  information file and the exact source archives of MPL-2.0 dependencies. See
  [licenses/README.md](licenses/README.md).
- Every release file, including `SHA256SUMS`, carries
  [GitHub artifact attestations](https://docs.github.com/en/actions/how-tos/secure-your-work/use-artifact-attestations/use-artifact-attestations)
  that link them to the workflow run and commit that built them.

Windows installers are not Authenticode-signed, so SmartScreen may show a warning
the first time you run one. The checksum and attestation are how you confirm the
file is genuine.

## Verify a download

**Checksums.** Compare the file's hash with the matching line in the release's
`SHA256SUMS`:

```powershell
Get-FileHash -Algorithm SHA256 .\m-dash_VERSION_x64-setup.exe
```

```sh
shasum -a 256 ./m-dash_VERSION_aarch64.dmg
```

**Attestation.** With the [GitHub CLI](https://cli.github.com/):

```sh
gh attestation verify m-dash_VERSION_x64-setup.exe --repo chuck-quypto/m-dash
gh attestation verify m-dash_VERSION_aarch64.dmg --repo chuck-quypto/m-dash
```

Check that the reported workflow and commit match the release tag. Run the same
command on `SHA256SUMS` to verify the checksum list itself.

**Signature (macOS).** Gatekeeper checks the Developer ID signature and notarization
when you open the app. To check by hand:

```sh
codesign --verify --deep --strict /Applications/m-dash.app
spctl --assess --type execute --verbose /Applications/m-dash.app
```

Provenance and signatures establish where a file came from and that it wasn't
altered. They don't establish that the software is bug-free.

## Release checklist for maintainers

1. Date the version's section in [CHANGELOG.md](CHANGELOG.md) and synchronize the
   version everywhere. Merge to `main` once CI is green.
2. Run the Release workflow on `main` with that commit's full SHA, and approve the
   `release` environment for the Mac signing job.
3. The workflow opens a draft release with both installers, per-platform release
   materials, `SHA256SUMS` and the attestation bundle.
4. Install each artifact on a clean machine: first launch with no accounts, one
   provider, missing CLI, restart, upgrade with retained data, and uninstall.
5. Publish the draft, which tags the commit `v<version>`. Never reuse a version or
   a published filename for different bytes.
