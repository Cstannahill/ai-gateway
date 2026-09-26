import test from "node:test";
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { once } from "node:events";
import { proxyLlama } from "../src/lib/llama-proxy.ts";

test("llama proxy enforces route, origin, UUID, query and JSON rules", async () => {
  const base = "http://127.0.0.1:3000/api/llama-server/";
  const headers = { host: "127.0.0.1:3000", origin: "http://127.0.0.1:3000", "sec-fetch-site": "same-origin" };
  const call = (path: string, method = "GET", body?: string, extra: Record<string, string> = {}) =>
    proxyLlama(new Request(base + path, { method, headers: { ...headers, ...extra }, body }), path.split("?")[0].split("/"), "http://127.0.0.1:8999");
  assert.equal((await call("models/not-a-uuid", "DELETE")).status, 400);
  assert.equal((await call("logs?after=-1")).status, 400);
  assert.equal((await call("logs?limit=2001")).status, 400);
  assert.equal((await call("status?junk=1")).status, 400);
  assert.equal((await call("start", "DELETE")).status, 405);
  assert.equal((await call("../health")).status, 404);
  assert.equal((await call("start", "POST", "{}", { "content-type": "text/plain" })).status, 415);
  assert.equal((await call("start", "POST", "{", { "content-type": "application/json" })).status, 400);
  assert.equal((await call("start", "POST", JSON.stringify({ huge: "x".repeat(65536) }), { "content-type": "application/json" })).status, 413);
  assert.equal((await call("status", "GET", undefined, { origin: "https://evil.test" })).status, 403);
  assert.equal((await call("status", "GET", undefined, { "sec-fetch-site": "cross-site" })).status, 403);
  assert.equal((await proxyLlama(new Request(base + "status"), ["status"], "http://127.0.0.1:8999")).status, 403);
});

test("llama proxy sends only fresh allowlisted headers and preserves upstream responses", async () => {
  const seen: { headers: Record<string, unknown>; url: string; method: string; body: string }[] = [];
  const server = createServer((req, res) => {
    const chunks: Buffer[] = [];
    req.on("data", (chunk) => chunks.push(chunk));
    req.on("end", () => {
      seen.push({ headers: req.headers, url: req.url ?? "", method: req.method ?? "", body: Buffer.concat(chunks).toString() });
      if (req.url === "/llama-server/models") { res.writeHead(201, { "content-type": "application/json" }); res.end('{"id":"ok"}'); }
      else if (req.url === "/llama-server/logs") { res.writeHead(302, { location: "/llama-server/status" }); res.end(); }
      else if (req.url === "/llama-server/restart") { setTimeout(() => { res.writeHead(202, { "content-type": "application/json" }); res.end("{}"); }, 100); }
      else if (req.url === "/llama-server/stop") { res.writeHead(400, { "content-type": "text/plain" }); res.end("bad request"); }
      else { res.writeHead(200, { "content-type": "application/json" }); res.end('{"enabled":false,"state":"disabled","message":"off"}'); }
    });
  });
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  const address = server.address();
  assert.ok(address && typeof address === "object");
  const upstream = `http://127.0.0.1:${address.port}`;
  const request = (path: string, method = "GET", body?: object) => new Request(`http://127.0.0.1:3000/api/llama-server/${path}`, {
    method,
    headers: { host: "127.0.0.1:3000", origin: "http://127.0.0.1:3000", "sec-fetch-site": "same-origin", referer: "http://127.0.0.1:3000/", cookie: "secret=1", authorization: "Bearer secret", ...(body ? { "content-type": "application/json" } : {}) },
    body: body ? JSON.stringify(body) : undefined,
  });
  try {
    const created = await proxyLlama(request("models", "POST", { alias: "test" }), ["models"], upstream);
    assert.equal(created.status, 201);
    assert.deepEqual(await created.json(), { id: "ok" });
    assert.equal(seen[0].method, "POST");
    assert.equal(seen[0].body, '{"alias":"test"}');
    for (const name of ["origin", "referer", "cookie", "authorization", "sec-fetch-site", "sec-fetch-mode", "sec-fetch-dest"]) assert.equal(seen[0].headers[name], undefined, name);
    assert.equal(seen[0].headers.host, `127.0.0.1:${address.port}`);
    assert.equal((await proxyLlama(request("status"), ["status"], upstream)).status, 200);
    assert.equal((await proxyLlama(request("logs"), ["logs"], upstream)).status, 502);
    assert.equal((await proxyLlama(request("restart", "POST"), ["restart"], upstream, 10)).status, 502);
    const plain = await proxyLlama(request("stop", "POST"), ["stop"], upstream);
    assert.equal(plain.status, 400);
    assert.equal(await plain.text(), "bad request");
  } finally {
    server.closeAllConnections();
    await new Promise<void>((resolve) => server.close(() => resolve()));
  }
});
