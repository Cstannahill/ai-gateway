import test from "node:test";
import assert from "node:assert/strict";
import {
  freshSession,
  makePayload,
  importChat,
  groupTurns,
  markdownExport,
  tokenCount,
  type ChatMessage,
} from "../src/lib/chat-session.ts";
import { emptyStream } from "../src/lib/chat-protocol.ts";
const message = (
  id: string,
  role: "user" | "assistant",
  content: string,
  model?: string,
  status: ChatMessage["status"] = "complete",
): ChatMessage => ({
  ...emptyStream(),
  id,
  turnId: "turn",
  role,
  content,
  requestedModel: model,
  status,
});
test("parallel model contexts share user prompts but never other model answers or partial output", () => {
  const session = freshSession("test");
  session.messages = [
    message("u", "user", "Hello"),
    message("a", "assistant", "Answer A", "A"),
    message("b", "assistant", "Answer B", "B"),
    message("c", "assistant", "Partial", "A", "error"),
  ];
  const a = makePayload(session.settings, "A", session.messages);
  assert.deepEqual(
    a.messages.map((m) => m.content),
    ["Hello", "Answer A"],
  );
  assert.deepEqual(
    makePayload(session.settings, "B", session.messages).messages.map(
      (m) => m.content,
    ),
    ["Hello", "Answer B"],
  );
  assert.deepEqual(
    makePayload(session.settings, "C", session.messages).messages.map(
      (m) => m.content,
    ),
    ["Hello"],
  );
});
test("chat imports validate, isolate IDs, stop unfinished generations, and never import an executable request", () => {
  const session = freshSession("original");
  session.models = ["A", "B"];
  session.settings.model = "A";
  session.messages = [
    message("u", "user", "Hello"),
    message("a", "assistant", "A", "A", "streaming"),
  ];
  session.messages[1].requestId = "0199768a-6e00-7000-8000-000000000001";
  const result = importChat(
    JSON.stringify({ format: "ai-gateway-chat-v2", mode: "demo", ...session }),
  );
  assert.notEqual(result.id, session.id);
  assert.equal(result.messages[1].status, "stopped");
  assert.equal(result.messages[1].requestId, undefined);
  assert.equal(result.messages[1].imported, true);
  assert.equal(result.messages[0].turnId, result.messages[1].turnId);
  assert.throws(() => importChat('{"format":"unknown"}'), /format/);
  assert.throws(
    () =>
      importChat(
        JSON.stringify({
          format: "ai-gateway-chat-v2",
          ...session,
          messages: [{ role: "tool", content: "x" }],
        }),
      ),
    /message/i,
  );
  assert.throws(() => importChat("x".repeat(8 * 1024 * 1024 + 1)), /large/);
});
test("legacy exports migrate to model lanes; missing tokens stay unknown and zero stays zero", () => {
  const session = freshSession("old");
  session.settings.model = "A";
  session.messages = [
    message("u", "user", "Prompt"),
    message("a", "assistant", "Answer", "A"),
  ];
  const result = importChat(
    JSON.stringify({
      format: "ai-gateway-chat-v1",
      ...session,
      models: undefined,
    }),
  );
  assert.deepEqual(result.models, ["A"]);
  assert.equal(groupTurns(result.messages).length, 1);
  assert.match(markdownExport(result), /Prompt/);
  assert.match(markdownExport(result), /Answer/);
  assert.equal(tokenCount(undefined), null);
  assert.equal(tokenCount({ total_tokens: 0 }), 0);
  assert.equal(tokenCount({ total_tokens: "unknown" }), null);
});
