import test from "node:test";
import assert from "node:assert/strict";
import {
  validateChatRequest,
  SseDecoder,
  applyChatEvent,
  emptyStream,
  readChatStream,
} from "../src/lib/chat-protocol.ts";

const payload = {
  model: "coding-fast",
  messages: [{ role: "user", content: "Hello" }],
  stream: true,
  n: 1,
};
test("chat requires an explicit model and admits only supported text generation parameters", () => {
  assert.deepEqual(validateChatRequest(payload), payload);
  assert.throws(
    () => validateChatRequest({ ...payload, model: " " }),
    /model/i,
  );
  assert.throws(
    () => validateChatRequest({ ...payload, tools: [] }),
    /Unsupported/,
  );
  assert.throws(
    () => validateChatRequest({ ...payload, request_id: "spoof" }),
    /Unsupported/,
  );
  assert.throws(
    () =>
      validateChatRequest({
        ...payload,
        messages: [{ role: "tool", content: "x" }],
      }),
    /message/i,
  );
  assert.throws(
    () => validateChatRequest({ ...payload, temperature: 3 }),
    /temperature/,
  );
  assert.throws(
    () => validateChatRequest({ ...payload, max_completion_tokens: 0 }),
    /tokens/,
  );
  assert.throws(() => validateChatRequest({ ...payload, n: 2 }), /single/);
});
test("SSE decoder handles chunk boundaries, CRLF, comments, and multiline data", () => {
  const decoder = new SseDecoder();
  const frames: string[] = [];
  for (const char of ': heartbeat\r\ndata: {"a":\r\ndata: 1}\r\n\r\ndata: [DONE]\n\n')
    frames.push(...decoder.push(char));
  assert.deepEqual(frames, ['{"a":\n1}', "[DONE]"]);
  const large = "data: " + "x".repeat(700000) + "\n\n";
  assert.equal(new SseDecoder().push(large + large).length, 2);
  assert.throws(
    () => new SseDecoder().push("x".repeat(1024 * 1024 + 1)),
    /large/,
  );
});
test("stream merges content and reasoning, replaces cumulative usage, and uses the gateway body ID", () => {
  const event = (extra: object) =>
    JSON.stringify({
      id: "chatcmpl-0199768a-6e00-7000-8000-000000000001",
      model: "resolved",
      choices: [],
      ...extra,
    });
  let state = applyChatEvent(
    emptyStream(),
    event({
      choices: [
        {
          index: 0,
          delta: { content: "Hi", reasoning_content: "Thinking" },
          finish_reason: null,
        },
      ],
    }),
  );
  state = applyChatEvent(
    state,
    event({
      usage: { prompt_tokens: 10, completion_tokens: 1, total_tokens: 11 },
    }),
  );
  state = applyChatEvent(
    state,
    event({
      usage: { prompt_tokens: 10, completion_tokens: 4, total_tokens: 14 },
    }),
  );
  state = applyChatEvent(
    state,
    event({ choices: [{ index: 0, delta: {}, finish_reason: "stop" }] }),
  );
  state = applyChatEvent(state, "[DONE]");
  assert.equal(state.content, "Hi");
  assert.equal(state.reasoning, "Thinking");
  assert.equal(state.usage?.total_tokens, 14);
  assert.equal(state.requestId, "0199768a-6e00-7000-8000-000000000001");
  assert.equal(state.done, true);
  assert.throws(() => applyChatEvent(emptyStream(), "[DONE]"), /completion/);
  assert.throws(() => applyChatEvent(emptyStream(), "{broken"), /JSON/);
});
test("stream preserves partial output when a provider reports failure and refuses truncated success", async () => {
  const frame = (v: unknown) => `data: ${JSON.stringify(v)}\n\n`;
  const first = frame({
    choices: [{ index: 0, delta: { content: "Partial ✓" } }],
  });
  let latest = emptyStream();
  const stream = (text: string) =>
    new Response(
      new ReadableStream({
        start(controller) {
          for (const byte of new TextEncoder().encode(text))
            controller.enqueue(new Uint8Array([byte]));
          controller.close();
        },
      }),
      { headers: { "Content-Type": "text/event-stream" } },
    );
  await assert.rejects(
    readChatStream(
      stream(
        first +
          frame({ error: { message: "Rate limited" } }) +
          "data: [DONE]\n\n",
      ),
      (s) => {
        latest = s;
      },
    ),
    /Rate limited/,
  );
  assert.equal(latest.content, "Partial ✓");
  await assert.rejects(
    readChatStream(stream(first), () => {}),
    /ended/,
  );
  await assert.rejects(
    readChatStream(new Response("{}"), () => {}),
    /event-stream/,
  );
});
