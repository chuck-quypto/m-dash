# Security policy

## Supported versions

Security fixes go into the latest published desktop release. Android, encrypted
sync and the relay are experimental and have no supported release; reports about
them are still welcome.

## Report a vulnerability privately

Use **Security → Report a vulnerability** on this repository to open a private
advisory. Do not put exploitable details, credentials, account data or pairing
codes in public issues, pull requests, discussions or screenshots.

If you can't use GitHub's private reporting, email
[m-dash@chuckdudley.net](mailto:m-dash@chuckdudley.net) instead. Don't send secrets
or exploit details by email until we've agreed how to exchange them.

Include the version or commit, platform, prerequisites, redacted steps and impact.
Use synthetic data. Never send real keys or a full usage database. There is no bug
bounty and no guaranteed response time; this is a single-maintainer project.

## Security boundaries

- Provider CLIs own subscription sign-in. m-dash reads their reported usage and
  does not extract login tokens.
- OpenRouter management keys have broader authority than m-dash's read operations
  need. Enter them only through the native secure prompt.
- m-dash does not encrypt its usage database. Protect it with OS account and disk
  controls.
- The renderer is local. The optional Claude feed posts accounting metadata to an
  authenticated receiver on the loopback interface, on fixed port 43183. It assumes
  a single-user computer: another local account that binds that port first could
  receive the feed's token and metadata. Leave the feed off on shared machines.
- Optional Claude ping submits a model request only after separate consent.
- Experimental mobile sync requires separate setup and consent, and cannot protect
  a compromised unlocked device.

See [privacy and storage](docs/PRIVACY.md) for details.

If you expose a credential, revoke or rotate it. Removing a value from the latest
file is not enough if it remains in history, logs, caches or artifacts.
