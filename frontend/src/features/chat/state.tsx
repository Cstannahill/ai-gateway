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
  type ChatStreamState,
} from "@/lib/chat-protocol";
import {
  freshSession,
  makePayload,
  modelSelection,
  MAX_MODELS,
  type ChatSettings,
  type ChatMessage,
  type ChatSession,
  type ChatDisplay,
} from "@/lib/chat-session";
export type {
  ChatMessage,
  ChatSession,
  ChatSettings,
} from "@/lib/chat-session";
interface ChatContextValue {
  sessions: ChatSession[];
  session: ChatSession;
  busy: boolean;
  activeStream?: string;
  display: ChatDisplay;
  setDisplay: (value: Partial<ChatDisplay>) => void;
  pins: string[];
  recent: string[];
  pin: (model: string) => void;
  select: (id: string) => void;
  newChat: () => void;
  remove: (id: string) => void;
  configure: (value: Partial<ChatSettings>) => void;
  models: (value: string[]) => void;
  draft: (text: string) => void;
  rename: (title: string) => void;
  send: (regenerate?: boolean | string) => Promise<void>;
  stop: (messageId?: string) => void;
  duplicate: () => void;
  importSession: (session: ChatSession) => void;
  editMessage: (id: string, content: string) => void;
  deleteMessage: (id: string) => void;
  clear: () => void;
  undo: () => void;
  canUndo: boolean;
  addFile: (name: string, text: string) => void;
  removeFile: (name: string) => void;
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
    live: [freshSession("live-start")],
    demo: [freshSession("demo-start")],
  });
  const [selected, setSelected] = useState({
    live: "live-start",
    demo: "demo-start",
  });
  const [activeStream, setActiveStream] = useState<string>();
  const running = useRef(new Map<string, AbortController>()),
    batchActive = useRef(false);
  const [undoValue, setUndoValue] = useState<{
    mode: string;
    session: ChatSession;
  }>();
  const [preferences, setPreferences] = useState<{
    pins: string[];
    recent: string[];
  }>({ pins: [], recent: [] });
  const [display, setDisplayValue] = useState<ChatDisplay>({
    layout: "columns",
    expand: false,
    metrics: true,
    color: "teal",
  });
  const sessions = records[mode],
    session = sessions.find((s) => s.id === selected[mode]) ?? sessions[0];
  useEffect(() => {
    try {
      const data = JSON.parse(
        localStorage.getItem("gateway-chat-models") ?? "{}",
      );
      if (Array.isArray(data.pins) && Array.isArray(data.recent)) {
        // Only model shortcuts persist; conversation content stays in memory.
        // eslint-disable-next-line react-hooks/set-state-in-effect
        setPreferences({
          pins: data.pins
            .filter((x: unknown) => typeof x === "string")
            .slice(0, 100),
          recent: data.recent
            .filter((x: unknown) => typeof x === "string")
            .slice(0, 20),
        });
      }
    } catch {
      /* Optional storage. */
    }
  }, []);
  useEffect(() => {
    const controllers = running.current;
    return () => {
      for (const c of controllers.values()) c.abort();
    };
  }, [mode]);
  const modify = (id: string, change: (s: ChatSession) => ChatSession) =>
    setRecords((all) => ({
      ...all,
      [mode]: all[mode].map((s) => (s.id === id ? change(s) : s)),
    }));
  function add(s: ChatSession) {
    setRecords((all) => ({ ...all, [mode]: [s, ...all[mode]] }));
    setSelected((all) => ({ ...all, [mode]: s.id }));
  }
  function remember(next: typeof preferences) {
    setPreferences(next);
    try {
      localStorage.setItem("gateway-chat-models", JSON.stringify(next));
    } catch {
      /* Optional storage. */
    }
  }
  function models(value: string[]) {
    if (batchActive.current) return;
    const chosen = modelSelection(value);
    modify(session.id, (s) => ({
      ...s,
      models: chosen,
      settings: { ...s.settings, model: chosen[0] ?? "" },
    }));
    remember({
      ...preferences,
      recent: [...new Set([...chosen, ...preferences.recent])].slice(0, 20),
    });
  }
  function stop(id?: string) {
    if (id) running.current.get(id)?.abort();
    else for (const c of running.current.values()) c.abort();
  }
  function remove(id: string) {
    if (activeStream === id) stop();
    setUndoValue({ mode, session: sessions.find((s) => s.id === id)! });
    setRecords((all) => {
      const rest = all[mode].filter((s) => s.id !== id);
      return {
        ...all,
        [mode]: rest.length
          ? rest
          : [
              freshSession(
                crypto.randomUUID(),
                session.settings,
                session.models,
              ),
            ],
      };
    });
  }
  function restore() {
    if (!undoValue || undoValue.mode !== mode || batchActive.current) return;
    const saved = undoValue.session;
    setRecords((all) => ({
      ...all,
      [mode]: [saved, ...all[mode].filter((s) => s.id !== saved.id)],
    }));
    setSelected((all) => ({ ...all, [mode]: saved.id }));
    setUndoValue(undefined);
  }
  function clear() {
    if (batchActive.current) return;
    setUndoValue({ mode, session });
    modify(session.id, (s) => ({
      ...s,
      messages: [],
      draft: "",
      files: [],
      title: "New conversation",
    }));
  }
  function deleteMessage(id: string) {
    if (batchActive.current) return;
    const item = session.messages.find((m) => m.id === id);
    if (!item) return;
    setUndoValue({ mode, session });
    modify(session.id, (s) => ({
      ...s,
      messages:
        item.role === "user"
          ? s.messages.filter((m) => m.turnId !== item.turnId)
          : s.messages.filter((m) => m.id !== id),
    }));
  }
  function editMessage(id: string, content: string) {
    if (batchActive.current || !content.trim()) return;
    const index = session.messages.findIndex((m) => m.id === id);
    if (index < 0) return;
    const item = session.messages[index];
    const messages = session.messages
      .slice(0, index + 1)
      .map((m) =>
        m.id === id
          ? {
              ...m,
              content,
              edited: true,
              usage: undefined,
              demoCost: undefined,
              requestId: undefined,
              sent: undefined,
              elapsedMs: undefined,
              finishReason: undefined,
              status: "complete" as const,
              done: true,
              error: undefined,
            }
          : m,
      );
    add({
      ...session,
      id: crypto.randomUUID(),
      title: session.title + " · edited",
      messages: item.role === "user" ? messages.slice(0, -1) : messages,
      draft: item.role === "user" ? content : "",
    });
  }
  async function send(regenerate: boolean | string = false) {
    if (batchActive.current || (!regenerate && !session.draft.trim())) return;
    let target = session,
      history: ChatMessage[],
      chosen = session.models;
    if (regenerate) {
      const answer =
        typeof regenerate === "string"
          ? session.messages.find((m) => m.id === regenerate)
          : [...session.messages].reverse().find((m) => m.role === "assistant");
      if (!answer?.requestedModel) return;
      const turnUser = session.messages.findIndex(
        (m) => m.turnId === answer.turnId && m.role === "user",
      );
      if (turnUser < 0) return;
      history = session.messages.slice(0, turnUser + 1);
      chosen = [answer.requestedModel];
      target = {
        ...session,
        id: crypto.randomUUID(),
        title: session.title + " · retry",
        messages: history,
        models: chosen,
        settings: { ...session.settings, model: chosen[0] },
        draft: "",
      };
      add(target);
    } else {
      if (!chosen.length || chosen.length > MAX_MODELS) return;
      const turnId = crypto.randomUUID();
      history = [
        ...session.messages,
        {
          ...emptyStream(),
          id: crypto.randomUUID(),
          turnId,
          role: "user",
          content: session.draft.trim(),
          status: "complete",
          done: true,
        },
      ];
    }
    batchActive.current = true;
    setUndoValue(undefined);
    setActiveStream(target.id);
    const turnId = history[history.length - 1].turnId;
    const answers: ChatMessage[] = chosen.map((model) => ({
      ...emptyStream(),
      id: crypto.randomUUID(),
      turnId,
      role: "assistant",
      requestedModel: model,
      status: "streaming",
    }));
    modify(target.id, (s) => ({
      ...s,
      title: s.messages.length
        ? s.title
        : history[history.length - 1].content.slice(0, 48),
      messages: [...history, ...answers],
      draft: "",
    }));
    for (const answer of answers)
      running.current.set(answer.id, new AbortController());
    await Promise.allSettled(
      answers.map(async (answer, index) => {
        const controller = running.current.get(answer.id)!,
          started = performance.now();
        const update = (patch: Partial<ChatMessage> | ChatStreamState) =>
          modify(target.id, (s) => ({
            ...s,
            messages: s.messages.map((m) =>
              m.id === answer.id ? { ...m, ...patch } : m,
            ),
          }));
        try {
          const payload = makePayload(
            target.settings,
            answer.requestedModel!,
            history,
          );
          update({ sent: payload });
          if (mode === "demo") {
            const text = `This is a **demonstration response** from **${answer.requestedModel}**. No request was sent to a model.\n\n### ${["Start with clear boundaries", "Make the tradeoffs explicit", "Keep the evidence close", "Design for recovery"][index]}\n\n${["Keep each model’s conversation independent while sending the same prompt to every selected model. A small, focused design is easier to test.", "Compare the same prompt across models, then inspect differences. One failed stream should not interrupt the others.", "Separate observations from interpretation. Preserve partial output and show missing metrics as unknown.", "Use cancellation and bounded requests. Keep the original answer when editing or regenerating so comparisons stay meaningful."][index]}\n\n| Signal | What to inspect |\n|---|---|\n| Response | Correctness and completeness |\n| Reasoning | Provider-returned reasoning |\n| Usage | Latest token observation |\n\n<details><summary>Assumptions and unresolved tradeoffs</summary>\n\nThis is illustrative content, not a live evaluation. Context windows and parameters depend on the selected provider.\n\n</details>\n\n\`\`\`ts\nconst request = { model: "${answer.requestedModel}", stream: true };\n\`\`\``;
            let content = "";
            for (const part of text.match(/.{1,48}|\n/g) ?? []) {
              await new Promise<void>((resolve, reject) => {
                if (controller.signal.aborted) {
                  reject(new Error("Stopped"));
                  return;
                }
                const abort = () => {
                  clearTimeout(timer);
                  reject(new Error("Stopped"));
                };
                const timer = setTimeout(
                  () => {
                    controller.signal.removeEventListener("abort", abort);
                    resolve();
                  },
                  22 + index * 6,
                );
                controller.signal.addEventListener("abort", abort, {
                  once: true,
                });
              });
              content += part;
              update({
                content,
                reasoning: `Demo reasoning only. This is a separate reasoning channel for ${answer.requestedModel}.\n\n1. Read the shared prompt.\n2. Consider this model’s own conversation history.\n3. Produce a response with explicit assumptions.\n\nNo other model’s answer enters this lane’s context.`,
                resolvedModel: answer.requestedModel,
              });
            }
            update({
              done: true,
              finishReason: "stop",
              usage: {
                prompt_tokens: 1240,
                completion_tokens: 362 + index * 71,
                total_tokens: 1602 + index * 71,
              },
              demoCost: [0.0029, 0.0035, 0.00083, 0][index],
            });
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
          update({
            status: "complete",
            elapsedMs: performance.now() - started,
          });
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
          running.current.delete(answer.id);
        }
      }),
    );
    batchActive.current = false;
    setActiveStream(undefined);
  }
  return (
    <Context.Provider
      value={{
        sessions,
        session,
        busy: !!activeStream,
        activeStream,
        display,
        setDisplay: (value) => setDisplayValue((v) => ({ ...v, ...value })),
        pins: preferences.pins,
        recent: preferences.recent,
        pin: (model) =>
          remember({
            ...preferences,
            pins: preferences.pins.includes(model)
              ? preferences.pins.filter((m) => m !== model)
              : [...preferences.pins, model].slice(-100),
          }),
        select: (id) => setSelected((all) => ({ ...all, [mode]: id })),
        newChat: () =>
          add(
            freshSession(crypto.randomUUID(), session.settings, session.models),
          ),
        remove,
        configure: (settings) => {
          if (!batchActive.current)
            modify(session.id, (s) => ({
              ...s,
              settings: { ...s.settings, ...settings },
            }));
        },
        models,
        draft: (text) => modify(session.id, (s) => ({ ...s, draft: text })),
        rename: (title) =>
          modify(session.id, (s) => ({
            ...s,
            title: title.trim().slice(0, 120) || "Untitled chat",
          })),
        send,
        stop,
        duplicate: () => {
          if (!batchActive.current)
            add({
              ...structuredClone(session),
              id: crypto.randomUUID(),
              title: session.title + " · copy",
            });
        },
        importSession: (s) => {
          if (!batchActive.current) add(s);
        },
        editMessage,
        deleteMessage,
        clear,
        undo: restore,
        canUndo: !!undoValue && undoValue.mode === mode && !activeStream,
        addFile: (name, text) =>
          modify(session.id, (s) => ({
            ...s,
            files: [
              ...s.files.filter((f) => f.name !== name),
              { name, text },
            ].slice(-10),
            draft:
              s.draft + `\n\n--- ${name} ---\n${text}\n--- end of ${name} ---`,
          })),
        removeFile: (name) =>
          modify(session.id, (s) => ({
            ...s,
            files: s.files.filter((f) => f.name !== name),
          })),
      }}
    >
      {children}
    </Context.Provider>
  );
}
