# Cloud sessions and other computers

> **You don't need any of this to use m-dash.** m-dash works fully on its own,
> with no Cloudflare account, relay or cloud service. A relay is only needed if
> you want usage from Claude Code cloud sessions, or from your other computers,
> to show up on the Allowance vs tokens page.

The **Allowance vs tokens** page compares how far each allowance moved with the
tokens m-dash saw. Usage that m-dash can't see shows up as *unattributed*. Two
common sources of that are Claude Code cloud sessions (claude.ai/code) and your
other computers. This guide sets up the optional **Cloud sessions and other
computers** feature so that usage is counted too.

It takes four parts, done once:

1. [Deploy your own relay](#1-deploy-your-relay) on Cloudflare (about 15 minutes).
2. [Create a usage channel](#2-create-a-usage-channel) in m-dash on your first computer.
3. [Join the channel](#3-join-from-your-other-computers) from your other computers.
4. [Set up your Claude cloud environment](#4-set-up-your-claude-cloud-environment).

Then [check that it works](#5-check-that-it-works). [Undoing it](#undo-it) is
described at the end.

**What it covers:** Claude Code cloud sessions, and Codex and Claude usage on
other computers running m-dash. **What it doesn't cover:** Codex cloud tasks, the
Claude and ChatGPT chat apps, and mobile apps. Those don't export per-request
usage, so they stay unattributed. Codex cloud usage still appears in the
account-wide daily totals that Codex reports.

**Requirements:** m-dash 0.5.4 or later on Windows or macOS (Linux builds can't
store the channel secret), a Cloudflare account, and Node.js 20 or later on the
computer you deploy from. Read [privacy and storage](PRIVACY.md) first. The relay
keeps per-request token counts, model and request identifiers for 14 days, and
drops prompts, content and email addresses before storing anything.

There is no hosted m-dash relay. You run your own, and only you hold its secrets.
m-dash works with a relay hosted anywhere that serves public HTTPS. This guide
uses the reference implementation in `relay/`, which runs on Cloudflare Workers
and D1 because they're cheap and quick to set up. To host it elsewhere, see
[Other hosts](#other-hosts).

## 1. Deploy your relay

If you already deployed `relay/` for the experimental Android companion, skip to
[Upgrading an existing relay](#upgrading-an-existing-relay).

1. Get the source and install the relay's tools:

   ```sh
   git clone https://github.com/chuck-quypto/m-dash.git
   cd m-dash/relay
   npm ci
   ```

2. Sign in to Cloudflare. A browser window opens:

   ```sh
   npx wrangler login
   ```

3. Create the database, and note the `database_id` it prints:

   ```sh
   npx wrangler d1 create m-dash-relay
   ```

4. Make your own copy of the configuration. `wrangler.local.jsonc` is ignored by
   Git, so your database ID stays out of the repository:

   ```sh
   cp wrangler.jsonc wrangler.local.jsonc
   ```

   In `wrangler.local.jsonc`, set `database_id` to the ID from step 3, and set
   `database_name` to `m-dash-relay`. You can also change `name`, which becomes
   part of your relay's address.

5. Create the tables. A new database needs only `schema.sql`. Don't run the files
   in `migrations/` on a new database:

   ```sh
   npx wrangler d1 execute m-dash-relay --remote --file=schema.sql --config wrangler.local.jsonc
   ```

6. Create the relay's two secrets. Run this command twice and keep both outputs
   somewhere private, such as a password manager:

   ```sh
   node -e "console.log(require('crypto').randomBytes(32).toString('base64url'))"
   ```

   The first output is the **enrollment secret**, which you'll type into m-dash.
   The second is the **epoch**. Store each one in Cloudflare; each command asks
   you to paste the value:

   ```sh
   npx wrangler secret put ENROLLMENT_TOKEN --config wrangler.local.jsonc
   npx wrangler secret put EPOCH --config wrangler.local.jsonc
   ```

   Never put either value in a file in the repository, a URL or a chat.

7. Deploy:

   ```sh
   npx wrangler deploy --config wrangler.local.jsonc
   ```

   Wrangler prints your relay's address, such as
   `https://m-dash-relay-dev.<your-subdomain>.workers.dev`. That is your **relay URL**.

8. Check it. This request carries no secret, so a working relay answers
   `HTTP/2 401`:

   ```sh
   curl -i https://YOUR-RELAY-URL/v1/usage
   ```

   A 503 means a secret is missing (step 6). A 404 or a Cloudflare error page
   means the address is wrong or the deploy failed.

### Upgrading an existing relay

A relay deployed before usage channels existed needs the usage tables. Apply the
migration once, then deploy the current `worker.mjs`:

```sh
npx wrangler d1 execute m-dash-relay --remote --file=migrations/0003_usage_channels.sql --config wrangler.local.jsonc
npx wrangler deploy --config wrangler.local.jsonc
```

If the relay is older than pairing v2, follow the coordinated upgrade in the
[sync protocol](development/SYNC_PROTOCOL.md) first. That includes `0002`, and the
Android phone must pair again afterwards.

## 2. Create a usage channel

Do this on one computer only. That computer creates the channel and the others
join it.

1. Open m-dash and go to **Settings → Cloud sessions and other computers**.
2. Enter your **Relay URL**. It may already be filled in if this computer was set
   up for the mobile companion.
3. Enter your **Enrollment secret** from step 6. Leave it empty only if this
   computer already has relay setup saved for that same URL.
4. Click **Create usage channel**.

The panel then shows **Connected to** your relay, along with two things to copy:

- **Copy setup script**: for your Claude cloud environment (part 4).
- **Copy code**: a join code for your other computers (part 3).

Both contain secrets for this channel. Anyone with the setup script can add
usage to the channel, and anyone with the code can read your shared usage. Keep
them private, as you would a password. A relay holds at most four channels.

## 3. Join from your other computers

On each other computer running m-dash:

1. Open **Settings → Cloud sessions and other computers**.
2. Paste the join code from part 2 into **Join a usage channel**.
3. Click **Join**.

Don't create a second channel on these computers. Every computer then shares
its per-request usage with the others, and each one shows the combined usage.

## 4. Set up your Claude cloud environment

Claude Code cloud sessions run in a *cloud environment*, which holds their
network rules and setup. Each environment needs this setup once, and every
session in it, in any project or repository, then reports its usage. Most
people only have the **Default** environment.

1. Go to [claude.ai/code](https://claude.ai/code) and click the environment name
   shown under the prompt box, next to the repository picker. Then click the gear
   next to the environment. (In a Claude project, the same editor is in the
   project settings, under the cloud environment menu.)
2. In **Setup script**, paste the script from **Copy setup script**. If the
   environment already has a setup script, add this one at the end.
3. Under **Network access**, if the level is *Limited* (*Trusted* in older
   versions), add your relay's host to **Allowed domains**, for example
   `m-dash-relay-dev.your-subdomain.workers.dev`, without `https://` or a path.
   Leave **Allow package managers** ticked. Skip this step if the level is
   unrestricted.
4. Save.

Only sessions started after you save are covered. Sessions that are already
running don't change.

> **Why a setup script and not environment variables?** Cloud environments
> silently drop variables whose names start with `OTEL_`, which is how Claude
> Code is normally told where to send usage. The script writes the same settings
> into Claude Code's own settings file (`~/.claude/settings.json` in the session)
> before Claude starts. Versions before 0.5.4-rc.2 gave four variables instead,
> which never reached the session. If you added those, you can delete them.

## 5. Check that it works

1. Start a new cloud session at claude.ai/code and give it any small task.
2. In m-dash, click **Refresh accounts**, which syncs the relay right away.
3. Open **Allowance vs tokens → Claude → Observed usage**. A row with
   **Where = Cloud session** means everything works. That row is always listed,
   however small.

If no row appears after a refresh:

| What you see | What to check |
| --- | --- |
| **Last exchange:** with an error, in the Settings panel | The relay or this computer's network. The message names the problem. |
| No error, no Cloud session row | The setup script is saved in the environment the session used; the session started after you saved; the relay host is in **Allowed domains**. |
| You only added the four `OTEL_…` variables | They don't work. Use the setup script (part 4). |
| Rows from your other computers are missing | That computer joined with the code (part 3) and m-dash is running on it. |

## Other hosts

m-dash doesn't depend on any Cloudflare feature. The relay is a small HTTPS and
JSON service with one SQL table pair, so it can run on AWS (Lambda with DynamoDB
or a SQL database), Azure Functions, Google Cloud Run, or a server of your own.
m-dash accepts any public `https://` origin with no path, port, IP address or
localhost. Port `relay/worker.mjs`, keeping this contract:

| Request | Auth | Does |
| --- | --- | --- |
| `POST /v1/usage` with `{"id","ingestHash","readHash"}` | `Bearer` enrollment secret | Creates a channel (201, or 409 if it exists). Hashes are unpadded base64url SHA-256 of the 43-character tokens. |
| `POST /v1/usage/{id}/rows` with `{"rows":[…]}` | `Bearer` ingest token | Stores up to 200 rows, ignoring duplicates of the same account and key. |
| `GET /v1/usage/{id}/rows/{after}` | `Bearer` read token | Returns `{"rows":[{"seq",…}]}`: up to 200 rows after `seq`, in order. |
| `DELETE /v1/usage/{id}` | `Bearer` read token | Deletes the channel and its rows. |
| `POST /v1/usage/{id}/{ingest token}/logs` | Token in the path | Accepts OTLP JSON logs from Claude Code and keeps only `api_request` accounting fields. |

Refuse requests that carry a query string or an `Origin` header, answer 401 for
a wrong or missing token, and delete rows after 14 days. The exact row fields and
checks are in `usageRow` and `otlpRows` in `worker.mjs`, and `relay/test/` shows
the expected behavior. Keep the enrollment secret in your platform's secret
store. The mobile companion uses other routes on the same relay; see the
[sync protocol](development/SYNC_PROTOCOL.md).

## Undo it

- **One computer:** in **Settings → Cloud sessions and other computers**, click
  **Disconnect this computer**. The other computers keep the channel.
- **Everything:** click **Delete channel and shared rows**, which removes the
  channel and its rows from the relay. Then, in the cloud environment, remove the
  setup script lines and the allowed domain. New cloud sessions stop reporting.
- **The relay itself:** in the Cloudflare dashboard, delete the Worker and the D1
  database (or remove the service from wherever you host it). Cloudflare can keep D1 recovery history for up to 30 days. See
  [privacy and storage](PRIVACY.md).

Rows expire from the relay after 14 days either way. Usage already copied into
m-dash stays in its local history.
