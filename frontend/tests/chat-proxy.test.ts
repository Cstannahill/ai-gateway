import test from "node:test";
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { once } from "node:events";
import { proxyChat } from "../src/lib/chat-proxy.ts";

test(
  "chat proxy forwards only validated JSON, streams immediately, cancels upstream, and preserves errors",
  { timeout: 10000 },
  async () => {
    let calls = 0,
      ended = false;
    const received: unknown[] = [];
    const server = createServer(async (req, res) => {
      calls++;
      assert.equal(req.url, "/v1/chat/completions");
      assert.equal(req.method, "POST");
      assert.equal(req.headers.authorization, undefined);
      assert.equal(req.headers.cookie, undefined);
      let raw = "";
      for await (const chunk of req) raw += chunk;
      const body = JSON.parse(raw);
      received.push(body);
      if (body.model === "error") {
        res.writeHead(429, { "Content-Type": "application/json" });
        res.end('{"error":{"message":"Provider quota reached"}}');
        return;
      }
      if (body.model === "redirect") {
        res.writeHead(302, { Location: "/other" });
        res.end();
        return;
      }
      res.writeHead(200, {
        "Content-Type": "text/event-stream",
        "Set-Cookie": "hidden=1",
      });
      res.write(
        'data: {"choices":[{"index":0,"delta":{"content":"first"}}]}\n\n',
      );
      // Remain open to prove first-byte delivery without buffering the response.
      res.on("close", () => {
        ended = true;
      });
    });
    server.listen(0, "127.0.0.1");
    await once(server, "listening");
    const address = server.address();
    assert.ok(address && typeof address === "object");
    const origin = `http://127.0.0.1:${address.port}`;
    const make = (model = "test", extra = {}, headers = {}) =>
      new Request("http://console.test/api/chat", {
        method: "POST",
        headers: {
          Origin: "http://console.test",
          "Content-Type": "application/json",
          Authorization: "secret",
          Cookie: "secret=1",
          ...headers,
        },
        body: JSON.stringify({
          model,
          messages: [{ role: "user", content: "hello" }],
          stream: true,
          n: 1,
          ...extra,
        }),
      });
    try {
      assert.equal(
        (
          await proxyChat(
            make("test", {}, { Origin: "https://other.test" }),
            origin,
          )
        ).status,
        403,
      );
      assert.equal(
        (await proxyChat(make("test", { tools: [] }), origin)).status,
        400,
      );
      assert.equal(
        (
          await proxyChat(
            make("test", {
              messages: [{ role: "user", content: "x".repeat(1024 * 1024) }],
            }),
            origin,
          )
        ).status,
        413,
      );
      const normalized = new Request("http://localhost:3000/api/chat", {
        method: "POST",
        headers: {
          Origin: "http://127.0.0.1:3000",
          Host: "127.0.0.1:3000",
          "Content-Type": "application/json",
        },
        body: JSON.stringify({
          model: "test",
          messages: [{ role: "user", content: "hello" }],
          stream: true,
          n: 2,
        }),
      });
      assert.equal((await proxyChat(normalized, origin)).status, 400);
      assert.equal(calls, 0);
      const response = await proxyChat(make(), origin);
      assert.equal(response.status, 200);
      assert.equal(response.headers.get("set-cookie"), null);
      assert.equal(response.headers.get("cache-control"), "no-store");
      const reader = response.body!.getReader();
      assert.match(
        new TextDecoder().decode((await reader.read()).value),
        /first/,
      );
      await reader.cancel();
      for (let i = 0; i < 30 && !ended; i++)
        await new Promise((r) => setTimeout(r, 10));
      assert.equal(ended, true);
      const error = await proxyChat(make("error"), origin);
      assert.equal(error.status, 429);
      assert.equal(
        (await error.json()).error.message,
        "Provider quota reached",
      );
      assert.equal((await proxyChat(make("redirect"), origin)).status, 502);
      assert.equal(received.length, 3);
    } finally {
      server.closeAllConnections();
      await new Promise<void>((resolve) => server.close(() => resolve()));
    }
  },
);
