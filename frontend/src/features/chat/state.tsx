"use client";
import {
  createContext,
  useContext,
  useEffect,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { useRuntime } from "@/components/runtime";
import {
  emptyStream,
  readChatStream,
  validateChatRequest,
  type ChatRequest,
  type ChatStreamState,
} from "@/lib/chat-protocol";

export interface ChatSettings {
  model: string;
  system: string;
  temperature: string;
  maxTokens: string;
  effort: "" | "low" | "medium" | "high";
}
export interface ChatMessage extends ChatStreamState {
  id: string;
  role: "user" | "assistant";
  status: "complete" | "streaming" | "stopped" | "error";
  requestedModel?: string;
  elapsedMs?: number;
  sent?: ChatRequest;
}
export interface ChatSession {
  id: string;
  title: string;
  settings: ChatSettings;
  messages: ChatMessage[];
  draft: string;
}
const defaults: ChatSettings = {
  model: "",
  system: "",
  temperature: "",
  maxTokens: "",
  effort: "",
};
const fresh = (id: string, settings = defaults): ChatSession => ({
  id,
  title: "New conversation",
  settings: { ...settings },
  messages: [],
  draft: "",
});
interface ChatContextValue {
  sessions: ChatSession[];
  session: ChatSession;
  busy: boolean;
  activeStream?: string;
  select: (id: string) => void;
  newChat: () => void;
  remove: (id: string) => void;
  configure: (settings: Partial<ChatSettings>) => void;
  draft: (text: string) => void;
  send: (regenerate?: boolean) => Promise<void>;
  stop: () => void;
}
const Context = createContext<ChatContextValue | null>(null);
export function useChat() {
  const value = useContext(Context);
  if (!value) throw new Error("Chat provider missing");
  return value;
}

export function ChatProvider({ children }: { children: ReactNode }) {
  const { mode } = useRuntime();
  const [records, setRecords] = useState({
    live: [fresh("live-start")],
    demo: [fresh("demo-start")],
  });
  const [selected, setSelected] = useState({
    live: "live-start",
    demo: "demo-start",
  });
  const [activeStream, setActiveStream] = useState<string>();
  const running = useRef<AbortController | null>(null);
  const sessions = records[mode],
    session = sessions.find((s) => s.id === selected[mode]) ?? sessions[0];
  useEffect(() => () => running.current?.abort(), [mode]);
  const modify = (id: string, change: (s: ChatSession) => ChatSession) =>
    setRecords((all) => ({
      ...all,
      [mode]: all[mode].map((s) => (s.id === id ? change(s) : s)),
    }));
  function add(s: ChatSession) {
    setRecords((all) => ({ ...all, [mode]: [s, ...all[mode]] }));
    setSelected((all) => ({ ...all, [mode]: s.id }));
  }
  function newChat() {
    add(fresh(crypto.randomUUID(), session.settings));
  }
  function remove(id: string) {
    if (activeStream === id) running.current?.abort();
    setRecords((all) => {
      const rest = all[mode].filter((s) => s.id !== id);
      return {
        ...all,
        [mode]: rest.length
          ? rest
          : [fresh(crypto.randomUUID(), session.settings)],
      };
    });
  }
  async function send(regenerate = false) {
    if (running.current || (!regenerate && !session.draft.trim())) return;
    const controller = new AbortController();
    running.current = controller;
    let target = session;
    if (regenerate) {
      // Regeneration branches: the original answer (including partial output) stays available.
      target = {
        ...session,
        id: crypto.randomUUID(),
        title: session.title + " · retry",
        messages: session.messages.slice(0, -1),
        draft: "",
      };
      add(target);
    }
    const user: ChatMessage = {
      ...emptyStream(),
      id: crypto.randomUUID(),
      role: "user",
      content: session.draft.trim(),
      status: "complete",
      done: true,
    };
    const history = regenerate ? target.messages : [...target.messages, user];
    const answerId = crypto.randomUUID(),
      settings = { ...session.settings };
    const answer: ChatMessage = {
      ...emptyStream(),
      id: answerId,
      role: "assistant",
      requestedModel: settings.model,
      status: "streaming",
    };
    modify(target.id, (s) => ({
      ...s,
      title: s.messages.length ? s.title : user.content.slice(0, 48),
      messages: [...history, answer],
      draft: "",
    }));
    setActiveStream(target.id);
    const started = performance.now();
    const update = (patch: Partial<ChatMessage>) =>
      modify(target.id, (s) => ({
        ...s,
        messages: s.messages.map((m) =>
          m.id === answerId ? { ...m, ...patch } : m,
        ),
      }));
    try {
      const payload = validateChatRequest({
        model: settings.model,
        stream: true,
        n: 1,
        messages: [
          ...(settings.system.trim()
            ? [{ role: "system", content: settings.system }]
            : []),
          ...history
            .filter(
              (m) =>
                m.role === "user" || (m.status === "complete" && m.content),
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
      update({ sent: payload });
      if (mode === "demo") {
        const text =
          'This is a **demonstration response**. No request was sent to a model.\n\nWhen you switch to live mode, your selected model or alias receives the conversation through AI Gateway.\n\n- Watch the answer arrive as a stream.\n- Expand reasoning when a provider returns it.\n- Follow **Inspect request** to investigate real generation attempts.\n\n```ts\nconst conversation = { model: "your-model", stream: true };\n```\n\nTry another turn, change models, or export this conversation.';
        let content = "";
        for (const part of text.match(/.{1,24}|\n/g) ?? []) {
          await new Promise<void>((resolve, reject) => {
            if (controller.signal.aborted) {
              reject(new Error("Stopped"));
              return;
            }
            const abort = () => {
              clearTimeout(timer);
              reject(new Error("Stopped"));
            };
            const timer = setTimeout(() => {
              controller.signal.removeEventListener("abort", abort);
              resolve();
            }, 30);
            controller.signal.addEventListener("abort", abort, { once: true });
          });
          content += part;
          update({
            content,
            reasoning:
              "Demo reasoning only. This text illustrates a separate reasoning channel.",
            resolvedModel: settings.model,
          });
        }
        update({ done: true, finishReason: "stop" });
      } else {
        const response = await fetch("/api/chat", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify(payload),
          signal: controller.signal,
        });
        if (!response.ok) {
          let message = `Chat request failed (HTTP ${response.status}).`;
          try {
            const json = await response.json();
            if (typeof json.error?.message === "string")
              message = json.error.message;
          } catch {
            /* Keep HTTP status. */
          }
          throw new Error(message);
        }
        await readChatStream(response, update);
      }
      update({ status: "complete", elapsedMs: performance.now() - started });
    } catch (e) {
      update({
        status: controller.signal.aborted ? "stopped" : "error",
        error: controller.signal.aborted
          ? undefined
          : e instanceof Error
            ? e.message
            : "Chat failed.",
        elapsedMs: performance.now() - started,
      });
    } finally {
      running.current = null;
      setActiveStream(undefined);
    }
  }
  return (
    <Context.Provider
      value={{
        sessions,
        session,
        busy: !!activeStream,
        activeStream,
        select: (id) => setSelected((all) => ({ ...all, [mode]: id })),
        newChat,
        remove,
        configure: (settings) =>
          modify(session.id, (s) => ({
            ...s,
            settings: { ...s.settings, ...settings },
          })),
        draft: (text) => modify(session.id, (s) => ({ ...s, draft: text })),
        send,
        stop: () => running.current?.abort(),
      }}
    >
      {children}
    </Context.Provider>
  );
}
