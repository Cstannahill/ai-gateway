export interface ChatRequest {
  model: string;
  messages: { role: "system" | "user" | "assistant"; content: string }[];
  stream: true;
  n: 1;
  temperature?: number;
  max_completion_tokens?: number;
  reasoning_effort?: "low" | "medium" | "high";
}
const object = (v: unknown): v is Record<string, unknown> =>
  typeof v === "object" && v !== null && !Array.isArray(v);
export function validateChatRequest(value: unknown): ChatRequest {
  if (!object(value)) throw new Error("Expected a JSON chat request.");
  const allowed = new Set([
    "model",
    "messages",
    "stream",
    "n",
    "temperature",
    "max_completion_tokens",
    "reasoning_effort",
  ]);
  if (Object.keys(value).some((k) => !allowed.has(k)))
    throw new Error("Unsupported chat parameter.");
  if (
    typeof value.model !== "string" ||
    !value.model.trim() ||
    value.model.length > 512
  )
    throw new Error("Select an explicit model or gateway alias.");
  if (value.stream !== true || value.n !== 1)
    throw new Error("Chat supports a single streamed completion.");
  if (
    !Array.isArray(value.messages) ||
    !value.messages.length ||
    value.messages.length > 256 ||
    !value.messages.every(
      (m) =>
        object(m) &&
        Object.keys(m).every((k) => k === "role" || k === "content") &&
        ["system", "user", "assistant"].includes(String(m.role)) &&
        typeof m.content === "string" &&
        m.content.length > 0,
    )
  )
    throw new Error(
      "Provide 1–256 nonempty text messages with supported roles.",
    );
  if (!value.messages.some((m) => m.role === "user"))
    throw new Error("A user message is required.");
  if (
    value.temperature !== undefined &&
    (typeof value.temperature !== "number" ||
      !Number.isFinite(value.temperature) ||
      value.temperature < 0 ||
      value.temperature > 2)
  )
    throw new Error("temperature must be between 0 and 2.");
  if (
    value.max_completion_tokens !== undefined &&
    (typeof value.max_completion_tokens !== "number" ||
      !Number.isSafeInteger(value.max_completion_tokens) ||
      value.max_completion_tokens < 1)
  )
    throw new Error("Maximum completion tokens must be a positive integer.");
  if (
    value.reasoning_effort !== undefined &&
    !["low", "medium", "high"].includes(String(value.reasoning_effort))
  )
    throw new Error("Invalid reasoning effort.");
  return { ...value, model: value.model.trim() } as unknown as ChatRequest;
}

export interface ChatStreamState {
  content: string;
  reasoning: string;
  resolvedModel?: string;
  requestId?: string;
  finishReason?: string;
  usage?: Record<string, unknown>;
  error?: string;
  done: boolean;
}
export const emptyStream = (): ChatStreamState => ({
  content: "",
  reasoning: "",
  done: false,
});

// Line-oriented parsing preserves CR/LF split across transport chunks.
export class SseDecoder {
  private buffer = "";
  private data: string[] = [];
  private size = 0;
  push(text: string): string[] {
    this.buffer += text;
    const events: string[] = [];
    while (true) {
      const match = /\r\n|\r|\n/.exec(this.buffer);
      if (
        !match ||
        (match[0] === "\r" && match.index === this.buffer.length - 1)
      )
        break;
      const line = this.buffer.slice(0, match.index);
      this.buffer = this.buffer.slice(match.index + match[0].length);
      if (line === "") {
        if (this.data.length) events.push(this.data.join("\n"));
        this.data = [];
        this.size = 0;
      } else if (line === "data" || line.startsWith("data:")) {
        const value = line.slice(5).replace(/^ /, "");
        this.data.push(value);
        this.size += new TextEncoder().encode(value).byteLength + 1;
        if (this.size > 1024 * 1024)
          throw new Error("Stream event is too large (1 MiB limit).");
      }
    }
    if (
      new TextEncoder().encode(this.buffer).byteLength + this.size >
      1024 * 1024
    )
      throw new Error("Stream event is too large (1 MiB limit).");
    return events;
  }
}
export function applyChatEvent(
  previous: ChatStreamState,
  data: string,
): ChatStreamState {
  if (data === "[DONE]") {
    if (!previous.finishReason || previous.error)
      throw new Error(
        previous.error ?? "Stream ended without a completion marker.",
      );
    return { ...previous, done: true };
  }
  let event: unknown;
  try {
    event = JSON.parse(data);
  } catch {
    throw new Error("Invalid JSON in the chat stream.");
  }
  if (!object(event)) throw new Error("Invalid chat stream event.");
  const next = { ...previous };
  if (typeof event.model === "string") next.resolvedModel = event.model;
  if (typeof event.id === "string") {
    const match =
      /^chatcmpl-([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})$/i.exec(
        event.id,
      );
    if (match) next.requestId = match[1];
  }
  if (object(event.error)) {
    next.error =
      typeof event.error.message === "string"
        ? event.error.message
        : "The provider reported a stream error.";
    return next;
  }
  if (object(event.usage)) next.usage = event.usage; // Cumulative observations replace, never add.
  if (!Array.isArray(event.choices))
    throw new Error("Chat stream has no choices array.");
  for (const choice of event.choices) {
    if (!object(choice) || choice.index !== 0 || !object(choice.delta))
      throw new Error("Unsupported chat choice in stream.");
    for (const [field, target] of [
      ["content", "content"],
      ["reasoning_content", "reasoning"],
    ] as const) {
      const value = choice.delta[field];
      if (value != null && typeof value !== "string")
        throw new Error("Invalid text delta.");
      if (typeof value === "string") next[target] += value;
    }
    if (choice.delta.tool_calls)
      throw new Error("This text chat does not execute tool calls.");
    if (choice.finish_reason != null) {
      if (typeof choice.finish_reason !== "string")
        throw new Error("Invalid completion marker.");
      next.finishReason = choice.finish_reason;
    }
  }
  return next;
}
export async function readChatStream(
  response: Response,
  update: (state: ChatStreamState) => void,
) {
  if (
    !response.headers.get("content-type")?.includes("text/event-stream") ||
    !response.body
  )
    throw new Error("Gateway did not return a text/event-stream response.");
  const reader = response.body.getReader(),
    decoder = new TextDecoder(),
    sse = new SseDecoder();
  let state = emptyStream(),
    bytes = 0;
  try {
    while (!state.done) {
      const { value, done } = await reader.read();
      if (done) break;
      bytes += value.byteLength;
      if (bytes > 8 * 1024 * 1024)
        throw new Error("Response exceeded the 8 MiB chat limit.");
      for (const data of sse.push(decoder.decode(value, { stream: true }))) {
        state = applyChatEvent(state, data);
        update(state);
        if (state.error) throw new Error(state.error);
        if (state.done) break;
      }
    }
    if (!state.done)
      throw new Error(
        "Connection ended before the response completed. Partial output is retained.",
      );
    return state;
  } finally {
    await reader.cancel().catch(() => {});
    reader.releaseLock();
  }
}
