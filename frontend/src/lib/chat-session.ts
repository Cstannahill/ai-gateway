import {
  emptyStream,
  validateChatRequest,
  type ChatRequest,
  type ChatStreamState,
} from "./chat-protocol.ts";
export interface ChatSettings {
  model: string;
  system: string;
  temperature: string;
  maxTokens: string;
  effort: "" | "low" | "medium" | "high";
}
export interface ChatMessage extends ChatStreamState {
  id: string;
  turnId: string;
  role: "user" | "assistant";
  status: "complete" | "streaming" | "stopped" | "error";
  requestedModel?: string;
  elapsedMs?: number;
  sent?: ChatRequest;
  edited?: boolean;
  imported?: boolean;
  demoCost?: number;
}
export interface SessionFile {
  name: string;
  text: string;
}
export interface ChatSession {
  id: string;
  title: string;
  settings: ChatSettings;
  models: string[];
  messages: ChatMessage[];
  draft: string;
  files: SessionFile[];
}
export interface ChatDisplay {
  layout: "list" | "columns";
  expand: boolean;
  metrics: boolean;
  color: string;
}
export const defaultSettings: ChatSettings = {
  model: "",
  system: "",
  temperature: "",
  maxTokens: "",
  effort: "",
};
export const MAX_MODELS = 4;
export const freshSession = (
  id: string,
  settings = defaultSettings,
  models: string[] = [],
): ChatSession => ({
  id,
  title: "New conversation",
  settings: { ...settings },
  models: [...models],
  messages: [],
  draft: "",
  files: [],
});
export function makePayload(
  settings: ChatSettings,
  model: string,
  messages: ChatMessage[],
): ChatRequest {
  return validateChatRequest({
    model,
    stream: true,
    n: 1,
    messages: [
      ...(settings.system.trim()
        ? [{ role: "system", content: settings.system }]
        : []),
      ...messages
        .filter(
          (m) =>
            m.role === "user" ||
            (m.requestedModel === model &&
              m.status === "complete" &&
              m.content),
        )
        .map((m) => ({ role: m.role, content: m.content })),
    ],
    ...(settings.temperature !== ""
      ? { temperature: Number(settings.temperature) }
      : {}),
    ...(settings.maxTokens !== ""
      ? { max_completion_tokens: Number(settings.maxTokens) }
      : {}),
    ...(settings.effort ? { reasoning_effort: settings.effort } : {}),
  });
}
export function groupTurns(messages: ChatMessage[]) {
  const groups: { id: string; user?: ChatMessage; answers: ChatMessage[] }[] =
    [];
  for (const message of messages) {
    let turn = groups.find((g) => g.id === message.turnId);
    if (!turn) {
      turn = { id: message.turnId, answers: [] };
      groups.push(turn);
    }
    if (message.role === "user") turn.user = message;
    else turn.answers.push(message);
  }
  return groups;
}
export function tokenCount(usage?: Record<string, unknown>): number | null {
  const value = usage?.total_tokens;
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0
    ? value
    : null;
}
export function markdownExport(session: ChatSession): string {
  return (
    `# ${session.title}\n\n` +
    session.messages
      .map(
        (m) =>
          `## ${m.role === "user" ? "You" : (m.requestedModel ?? "Assistant")}${m.edited ? " (edited)" : ""}\n\n${m.content}${m.reasoning ? "\n\n<details><summary>Reasoning</summary>\n\n" + m.reasoning + "\n\n</details>" : ""}\n\n`,
      )
      .join("")
  );
}
const record = (v: unknown): v is Record<string, unknown> =>
  !!v && typeof v === "object" && !Array.isArray(v);
const text = (v: unknown, fallback = "") =>
  typeof v === "string" ? v : fallback;
export function modelSelection(value: unknown): string[] {
  if (
    !Array.isArray(value) ||
    value.length > MAX_MODELS ||
    !value.every(
      (m) => typeof m === "string" && m.trim().length > 0 && m.length <= 512,
    )
  )
    throw new Error(`Select up to ${MAX_MODELS} nonempty model IDs.`);
  return [...new Set(value.map((m) => (m as string).trim()))];
}
export function importChat(raw: string): ChatSession {
  if (new TextEncoder().encode(raw).byteLength > 8 * 1024 * 1024)
    throw new Error("Import is too large (8 MiB limit).");
  const data: unknown = JSON.parse(raw);
  if (
    !record(data) ||
    !["ai-gateway-chat-v1", "ai-gateway-chat-v2"].includes(String(data.format))
  )
    throw new Error(
      "Unsupported conversation format. Use an AI Gateway JSON export.",
    );
  if (
    !record(data.settings) ||
    !Array.isArray(data.messages) ||
    data.messages.length > 2000
  )
    throw new Error("Invalid conversation settings or messages.");
  const settings: ChatSettings = {
    model: text(data.settings.model),
    system: text(data.settings.system),
    temperature: text(data.settings.temperature),
    maxTokens: text(data.settings.maxTokens),
    effort: ["", "low", "medium", "high"].includes(String(data.settings.effort))
      ? (data.settings.effort as ChatSettings["effort"])
      : "",
  };
  const models = modelSelection(
    data.models ?? (settings.model ? [settings.model] : []),
  );
  makePayload(settings, models[0] ?? "validation-only", [
    {
      ...emptyStream(),
      id: "check",
      turnId: "check",
      role: "user",
      status: "complete",
      content: "check",
    },
  ]);
  const session = freshSession(
    crypto.randomUUID(),
    { ...settings, model: models[0] ?? "" },
    models,
  );
  session.title =
    text(data.title, "Imported conversation").slice(0, 120) ||
    "Imported conversation";
  session.draft = text(data.draft);
  let turnId = "";
  session.messages = data.messages.map((m) => {
    if (
      !record(m) ||
      !["user", "assistant"].includes(String(m.role)) ||
      typeof m.content !== "string" ||
      (m.reasoning != null && typeof m.reasoning !== "string")
    )
      throw new Error(
        "Invalid imported message. Only text user/assistant messages are supported.",
      );
    if (m.role === "user") turnId = crypto.randomUUID();
    if (!turnId)
      throw new Error("Imported answers must follow a user message.");
    const status =
      m.status === "streaming" || m.status === "stopped"
        ? "stopped"
        : m.status === "error"
          ? "error"
          : "complete";
    return {
      ...emptyStream(),
      id: crypto.randomUUID(),
      turnId,
      role: m.role as "user" | "assistant",
      content: m.content,
      reasoning: text(m.reasoning),
      requestedModel:
        m.role === "assistant" ? text(m.requestedModel, models[0]) : undefined,
      status,
      done: status === "complete",
      imported: true,
      edited: m.edited === true,
      elapsedMs:
        typeof m.elapsedMs === "number" &&
        Number.isFinite(m.elapsedMs) &&
        m.elapsedMs >= 0
          ? m.elapsedMs
          : undefined,
      usage: record(m.usage) ? m.usage : undefined,
      finishReason: text(m.finishReason) || undefined,
      error: text(m.error) || undefined,
    };
  });
  // Imported IDs, costs and request payloads are not authoritative live telemetry.
  if (Array.isArray(data.files))
    session.files = data.files
      .filter(
        (f) =>
          record(f) && typeof f.name === "string" && typeof f.text === "string",
      )
      .slice(0, 10)
      .map((f) => ({
        name: String(f.name).slice(0, 200),
        text: String(f.text),
      }));
  return session;
}
