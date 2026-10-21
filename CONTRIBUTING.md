# Contributing

Thanks for helping improve m-dash. Windows x64 and Apple Silicon macOS are the
desktop release targets. Linux builds work for development. Android, encrypted
sync and the relay are experimental.

## Before you start

- **Bugs and provider breakage:** open an issue using the matching form. Provider
  CLIs and APIs change without notice, so include the CLI version and a redacted
  sample of what changed.
- **Features and larger changes:** open an issue or discussion first so we can
  agree on scope before you write code.
- **Security problems:** do not open a public issue. Follow [SECURITY.md](SECURITY.md).

## Making a change

Follow the [development instructions](docs/DEVELOPMENT.md). Keep changes focused
and add a small regression test for a behavior fix. Before opening a pull request,
run the checks that apply to your change:

```sh
npm run build
cargo test --manifest-path src-tauri/Cargo.toml --locked --lib -- --test-threads=1
cargo test --manifest-path sync-core/Cargo.toml --locked
cargo clippy --manifest-path src-tauri/Cargo.toml --locked --all-targets -- -D warnings
cargo fmt --manifest-path src-tauri/Cargo.toml --check
node scripts/check-release-docs.mjs
```

Pull requests run the same checks on Windows, macOS and Ubuntu. State which
platforms you tested by hand; browser previews do not prove installed behavior.

A dependency change also changes the generated license notices. Regenerate them as
described in [licenses/README.md](licenses/README.md), or say in the pull request
that you could not, and a maintainer will.

## Ground rules

- Use synthetic fixtures. Never commit credentials, databases, real account
  captures, pairing codes or signing material.
- Tests must not contact real provider accounts or relays, or make paid inference
  calls. Diagnostic examples can print private metrics; keep their output local.
- Preserve user data during failures and upgrades. Do not weaken freshness,
  account separation, secure storage or consent to make a test pass.
- Provider CLIs keep control of authentication. m-dash never extracts their login
  secrets.

## Sign your commits (DCO)

Contributions are accepted under the [Apache License 2.0](LICENSE), the same
license as the project. Each commit must include a `Signed-off-by` line certifying
the [Developer Certificate of Origin](https://developercertificate.org/): that you
wrote the change or otherwise have the right to submit it under this license.

```sh
git commit -s -m "Describe the change"
```

Third-party code must keep its license and attribution. CND assets have
[separate terms](BRAND-LICENSE.txt); do not imply CND or provider endorsement.

Everyone taking part follows the [code of conduct](CODE_OF_CONDUCT.md).
