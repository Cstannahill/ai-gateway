import test from "node:test";
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { once } from "node:events";
import { proxyRead } from "../src/lib/server-proxy.ts";

test("read proxy preserves JSON status, bounds bodies, refuses redirects, and never forwards browser credentials", async () => {
  let seenAuth: unknown, seenCookie: unknown;
  const server = createServer((req, res) => {
    seenAuth = req.headers.authorization;
    seenCookie = req.headers.cookie;
    if (req.url === "/health") {
      res.writeHead(200, { "Content-Type": "application/json" });
      res.end('{"status":"healthy"}');
    } else if (req.url === "/metrics") {
      res.writeHead(302, { Location: "/health" });
      res.end();
    } else if (req.url === "/providers") {
      res.writeHead(200);
      res.end("not json");
    } else if (req.url === "/v1/models") {
      res.writeHead(200);
      res.end(JSON.stringify({ data: ["x".repeat(8 * 1024 * 1024)] }));
    } else {
      res.writeHead(req.url?.includes("00000000") ? 404 : 503, {
        "Content-Type": "application/json",
      });
      res.end(
        '{"error":{"code":"analytics_unavailable","message":"Unavailable"}}',
      );
    }
  });
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  const address = server.address();
  assert.ok(address && typeof address === "object");
  const origin = `http://127.0.0.1:${address.port}`;
  const read = (path: string) =>
    proxyRead(
      new Request("http://console.test/api/gateway" + path, {
        headers: { Authorization: "secret", Cookie: "session=secret" },
      }),
      path.slice(1).split("/"),
      origin,
    );
  try {
    const health = await read("/health");
    assert.equal(health.status, 200);
    assert.equal(health.headers.get("cache-control"), "no-store");
    assert.equal(seenAuth, undefined);
    assert.equal(seenCookie, undefined);
    assert.equal((await read("/v1/analytics/requests")).status, 503);
    assert.equal(
      (
        await read(
          "/v1/analytics/requests/00000000-0000-0000-0000-000000000000",
        )
      ).status,
      404,
    );
    assert.equal((await read("/metrics")).status, 502);
    assert.equal((await read("/providers")).status, 502);
    assert.equal((await read("/v1/models")).status, 413);
    assert.equal(
      (
        await proxyRead(
          new Request("http://console.test/api/gateway/health"),
          ["health"],
          "http://user:pass@localhost",
        )
      ).status,
      500,
    );
  } finally {
    server.closeAllConnections();
    await new Promise<void>((resolve) => server.close(() => resolve()));
  }
});
