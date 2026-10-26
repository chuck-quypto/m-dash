# Experimental relay — outside desktop release scope

This Cloudflare Worker/D1 implementation is development code. No managed relay,
Android release or service availability is promised. Desktop users can leave the
Mobile companion panel unconfigured. Never point tests at someone else's relay.

`wrangler.jsonc` is a template with a zero UUID, not a working database binding.
Copy it to ignored `wrangler.local.jsonc` and supply your own D1 database ID after
reviewing Cloudflare costs, jurisdiction and recovery retention. Keep enrollment
and epoch secrets in the platform's secret store, never Git, a URL or command-line
arguments. Provider credentials never belong on the relay.

Read [the protocol](../docs/development/SYNC_PROTOCOL.md) and
[privacy](../docs/PRIVACY.md#optional-encrypted-mobile-sync-development) first.
The latest active ciphertext expires after seven days; D1 recovery copies can
persist longer. A new recovery epoch is required after database restoration.

Use `npm ci` and `npm test` from this directory for synthetic local tests. No
deployment is required. Operator provisioning, deployment, secret configuration,
desktop secure enrollment and real-device acceptance are separate operations.

The scoped npm override gives Miniflare the patched `undici` 7.30.1 release.
Remove it once Miniflare itself requires a patched version.

Known limits: the active-channel cap (four) is global to the deployment and shared
by every enrollment token, and rate limiting is per client IP only. There is no
operator route to list or delete channels; recovery uses an epoch rotation. Treat a
deployment as single-owner and keep enrollment secrets private.

The current source requires pairing v2 and a separate `reader_hash` column. A
previous deployment needs the coordinated upgrade and re-pairing described
in the protocol. The migration under `migrations/` applies only to an existing old
schema; `schema.sql` already includes the column for new databases. Never run both
on the same new database.
