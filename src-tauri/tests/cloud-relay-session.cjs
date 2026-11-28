// Synthetic Claude exporter. The launch environment and the setup process are independent.
const assert = require("node:assert/strict");
const path = require("node:path");
const { once } = require("node:events");

async function main() {
  const directory = path.join(process.env.HOME, ".claude");
  const settings = require(path.join(directory, "settings.json"));
  const env = { ...settings.env, ...process.env };
  const endpoint = new URL(env.OTEL_EXPORTER_OTLP_LOGS_ENDPOINT);
  assert.equal(endpoint.hostname, "127.0.0.1");
  const headers = { "content-type": "application/json" };
  for (const key of ["OTEL_EXPORTER_OTLP_HEADERS", "OTEL_EXPORTER_OTLP_LOGS_HEADERS"]) {
    if (env[key]) {
      const [name, value] = env[key].split("=");
      headers[name] = value;
    }
  }
  let listener;
  try {
    for (const group of settings.hooks?.SessionStart ?? []) {
      for (const hook of group.hooks) {
        const file = hook.command.match(/^node "\$HOME\/\.claude\/([\w.-]+)"/)[1];
        listener = require(path.join(directory, file));
        await once(listener, "listening");
      }
    }
    const response = await fetch(endpoint, {
      method: "POST", headers, body: '{"resourceLogs":[]}', redirect: "error",
      signal: AbortSignal.timeout(5000),
    });
    assert.equal(response.status, 200);
    await response.text();
  } finally {
    listener?.close();
  }
}
main().catch(error => { console.error(error); process.exitCode = 1; });
