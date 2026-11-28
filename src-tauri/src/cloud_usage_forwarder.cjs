const http = require("node:http");
const { listen, env } = require("./m-dash-usage-relay.json");
const local = new URL(listen);

// Claude can add arbitrary collector credentials at launch. Build a new HTTP request here.
const server = http.createServer(async (request, response) => {
  if (request.method !== "POST" || request.url !== local.pathname || request.headers.origin !== undefined) {
    response.writeHead(404).end();
    return;
  }
  const headers = { "content-type": "application/json" };
  const encoding = request.headers["content-encoding"];
  if (encoding === "gzip") headers["content-encoding"] = "gzip";
  else if (encoding && encoding !== "identity") {
    response.writeHead(415).end();
    return;
  }
  try {
    const chunks = [];
    let length = 0;
    for await (const chunk of request) {
      length += chunk.length;
      if (length > 256 * 1024) {
        response.writeHead(413).end();
        return;
      }
      chunks.push(chunk);
    }
    const result = await fetch(env.OTEL_EXPORTER_OTLP_LOGS_ENDPOINT, {
      method: "POST", headers, body: Buffer.concat(chunks), redirect: "error",
      signal: AbortSignal.timeout(10000),
    });
    await result.body?.cancel();
    response.writeHead(result.status, { "content-type": "application/json" }).end(result.ok ? "{}" : "");
  } catch {
    response.writeHead(502).end();
  }
});
module.exports = server.listen(Number(local.port), "127.0.0.1");
