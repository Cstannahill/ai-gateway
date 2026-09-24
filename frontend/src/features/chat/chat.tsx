"use client";
import { useEffect, useRef, useState } from "react";
import {
  ArrowDown,
  ArrowUp,
  Code2,
  Download,
  History,
  MessageSquare,
  Paperclip,
  Plus,
  Search,
  Sparkles,
  Square,
  Trash2,
  Undo2,
  X,
} from "lucide-react";
import { useRuntime } from "@/components/runtime";
import { groupTurns } from "@/lib/chat-session";
import { useChat } from "./state";
import { ModelPicker } from "./model-picker";
import { GenerationSettings, RoomControls } from "./controls";
import { Message, ReasoningSidebar } from "./message";
import { downloadChat } from "./shared";
import "./chat.css";
const starters = [
  {
    icon: Code2,
    title: "Build something",
    prompt:
      "Help me design a small, well-tested API client with streaming support.",
  },
  {
    icon: Sparkles,
    title: "Think it through",
    prompt:
      "What tradeoffs should I consider when routing requests across multiple AI providers?",
  },
  {
    icon: MessageSquare,
    title: "Explore an idea",
    prompt:
      "Explain time to first token versus end-to-end latency with a concrete example.",
  },
];
export function Chat() {
  const {
    sessions,
    session,
    select,
    newChat,
    remove,
    draft,
    busy,
    activeStream,
    send,
    stop,
    models,
    display,
    setDisplay,
    undo,
    canUndo,
    addFile,
  } = useChat();
  const { mode } = useRuntime();
  const [history, setHistory] = useState(false),
    [search, setSearch] = useState(""),
    [atBottom, setAtBottom] = useState(true),
    [reasoningId, setReasoningId] = useState<string>(),
    [notice, setNotice] = useState(""),
    [find, setFind] = useState(false),
    [messageSearch, setMessageSearch] = useState("");
  const viewport = useRef<HTMLDivElement>(null),
    composer = useRef<HTMLTextAreaElement>(null),
    fileInput = useRef<HTMLInputElement>(null),
    follow = useRef(true);
  const messages = session.messages,
    reasoning = messages.find((m) => m.id === reasoningId);
  const turns = groupTurns(messages).filter(
    (t) =>
      !messageSearch ||
      [t.user, ...t.answers].some(
        (m) =>
          m &&
          (m.content + " " + m.reasoning)
            .toLowerCase()
            .includes(messageSearch.toLowerCase()),
      ),
  );
  useEffect(() => {
    if (follow.current && viewport.current)
      viewport.current.scrollTop = viewport.current.scrollHeight;
  }, [messages]);
  useEffect(() => {
    follow.current = true;
    if (viewport.current)
      viewport.current.scrollTop = viewport.current.scrollHeight;
  }, [session.id]);
  function toggleReasoning(id?: string) {
    setReasoningId(id === reasoningId ? undefined : id);
  }
  async function attach(file?: File) {
    if (!file) return;
    try {
      if (file.size > 256 * 1024)
        throw new Error("Text attachments must be 256 KiB or smaller.");
      if (
        !/\.(txt|md|json|csv|log|ts|js|py|rs|html|css|yaml|yml)$/i.test(
          file.name,
        )
      )
        throw new Error("Choose a plain text or source-code file.");
      const text = await file.text();
      if (text.includes("\0"))
        throw new Error("Binary files are not supported.");
      addFile(file.name, text);
      setNotice("File text added to your draft. Review it before sending.");
      composer.current?.focus();
    } catch (e) {
      setNotice(e instanceof Error ? e.message : "Could not attach this file.");
    }
  }
  const canSend = !!session.draft.trim() && session.models.length > 0 && !busy;
  function submit() {
    if (canSend) {
      follow.current = true;
      void send();
    }
  }
  return (
    <div
      className={
        "chat-workspace multi-chat " +
        (reasoning ? "with-reasoning " : "") +
        (history ? "history-expanded" : "")
      }
      data-message-color={display.color}
    >
      <aside
        className={"chat-history " + (history ? "show-history" : "")}
        aria-label="Conversations"
      >
        <button
          className="primary chat-new"
          onClick={() => {
            newChat();
            setHistory(false);
          }}
        >
          <Plus size={15} />
          New chat
        </button>
        <label className="chat-history-search">
          <Search size={14} />
          <input
            aria-label="Search conversations"
            placeholder="Search conversations"
            value={search}
            onChange={(e) => setSearch(e.target.value)}
          />
        </label>
        <span className="nav-label">THIS SESSION · {sessions.length}</span>
        <div className="chat-conversations">
          {sessions
            .filter((s) => s.title.toLowerCase().includes(search.toLowerCase()))
            .map((s) => (
              <div key={s.id} className={s.id === session.id ? "selected" : ""}>
                <button
                  className="chat-session-select"
                  aria-pressed={s.id === session.id}
                  onClick={() => {
                    select(s.id);
                    setHistory(false);
                  }}
                >
                  <MessageSquare size={14} />
                  <span>{s.title}</span>
                  {s.id === activeStream && <span className="chat-pulse" />}
                </button>
                <button
                  className="chat-session-delete icon-button"
                  disabled={busy}
                  aria-label={`Delete conversation: ${s.title}`}
                  onClick={() => remove(s.id)}
                >
                  <Trash2 size={13} />
                </button>
              </div>
            ))}
        </div>
        <p className="chat-history-note">
          Conversations stay in this tab until you reload. Export a chat to keep
          a copy.
        </p>
      </aside>
      <section className="chat-main" aria-label="Chat workspace">
        <div className="chat-toolbar">
          <button
            className="chat-history-toggle icon-button"
            aria-label="Toggle conversations"
            aria-expanded={history}
            onClick={() => setHistory(!history)}
          >
            <History size={17} />
          </button>
          <ModelPicker />
          <div className="chat-toolbar-actions">
            <select
              aria-label="Response layout"
              value={display.layout}
              onChange={(e) =>
                setDisplay({ layout: e.target.value as typeof display.layout })
              }
            >
              <option value="list">List</option>
              <option value="columns">Side by side</option>
            </select>
            <button
              className="icon-button"
              aria-label="Toggle reasoning sidebar"
              aria-pressed={!!reasoning}
              disabled={
                !messages.some((m) => m.role === "assistant" && m.reasoning)
              }
              onClick={() => {
                if (reasoning) setReasoningId(undefined);
                else
                  setReasoningId(
                    [...messages]
                      .reverse()
                      .find((m) => m.role === "assistant" && m.reasoning)?.id,
                  );
              }}
            >
              <Sparkles size={16} />
            </button>
            <button
              className="icon-button"
              aria-label="Search messages"
              aria-expanded={find}
              onClick={() => {
                setFind(!find);
                if (find) setMessageSearch("");
              }}
            >
              <Search size={16} />
            </button>
            <button
              className="icon-button quick-chat-export"
              aria-label="Export conversation"
              disabled={!messages.length}
              onClick={() =>
                downloadChat(
                  `chat-${session.id}.json`,
                  JSON.stringify(
                    {
                      format: "ai-gateway-chat-v2",
                      mode,
                      exportedAt: new Date().toISOString(),
                      ...session,
                    },
                    null,
                    2,
                  ),
                )
              }
            >
              <Download size={16} />
            </button>
            <RoomControls onNotice={setNotice} />
          </div>
        </div>
        {session.models.length > 0 && (
          <div className="chat-selected-models">
            {session.models.map((m, i) => (
              <span className="chat-model-chip" key={m}>
                <span className={"model-dot dot-" + i} />
                <span title={m}>{m}</span>
                <button
                  className="icon-button"
                  aria-label={`Remove model ${m}`}
                  disabled={busy}
                  onClick={() =>
                    models(session.models.filter((id) => id !== m))
                  }
                >
                  <X size={12} />
                </button>
              </span>
            ))}
            <small>
              {session.models.length > 1
                ? `${session.models.length} independent responses per send`
                : "One model selected"}
            </small>
          </div>
        )}
        {find && (
          <label className="chat-find">
            <Search size={14} />
            <input
              aria-label="Find in conversation"
              placeholder="Find text or reasoning in this conversation…"
              value={messageSearch}
              onChange={(e) => setMessageSearch(e.target.value)}
            />
            <small>{turns.length} matching turns</small>
          </label>
        )}
        {(notice || canUndo) && (
          <div className="chat-action-notice" role="status">
            <span>{notice || "Conversation updated."}</span>
            {canUndo && (
              <button onClick={undo}>
                <Undo2 size={13} />
                Undo last removal
              </button>
            )}
            {notice && (
              <button
                className="icon-button"
                aria-label="Dismiss chat notice"
                onClick={() => setNotice("")}
              >
                <X size={13} />
              </button>
            )}
          </div>
        )}
        <div
          className="chat-transcript"
          ref={viewport}
          onScroll={() => {
            const v = viewport.current;
            if (v) {
              const bottom = v.scrollHeight - v.scrollTop - v.clientHeight < 90;
              follow.current = bottom;
              setAtBottom(bottom);
            }
          }}
        >
          {!messages.length ? (
            <div className="chat-empty">
              <div className="chat-empty-symbol">
                <MessageSquare size={28} />
              </div>
              <div className="eyebrow">YOUR MODELS. ONE CONVERSATION.</div>
              <h2>What are we working on?</h2>
              <p>
                Bring up to four models into the conversation.
                <br />
                Compare their answers. Follow their reasoning.
              </p>
              <div className="chat-starters">
                {starters.map((s) => (
                  <button
                    key={s.title}
                    onClick={() => {
                      draft(s.prompt);
                      composer.current?.focus();
                    }}
                  >
                    <s.icon size={17} />
                    <strong>{s.title}</strong>
                    <span>{s.prompt}</span>
                  </button>
                ))}
              </div>
            </div>
          ) : (
            <div className={"chat-turns layout-" + display.layout}>
              {turns.length ? (
                turns.map((turn) => (
                  <section
                    className="chat-turn"
                    key={turn.id}
                    aria-label="Conversation turn"
                  >
                    {turn.user && (
                      <Message
                        message={turn.user}
                        onReasoning={toggleReasoning}
                        reasoningOpen={false}
                      />
                    )}
                    <div
                      className={
                        "chat-answer-grid " +
                        (turn.answers.length > 1 ? "multiple" : "")
                      }
                      style={
                        {
                          "--answer-count": turn.answers.length,
                        } as React.CSSProperties
                      }
                    >
                      {turn.answers.map((m) => (
                        <Message
                          key={m.id}
                          message={m}
                          onReasoning={toggleReasoning}
                          reasoningOpen={reasoningId === m.id}
                        />
                      ))}
                    </div>
                  </section>
                ))
              ) : (
                <p className="chat-hint">No messages match this search.</p>
              )}
            </div>
          )}
        </div>
        <div className="chat-compose-area">
          {!atBottom && messages.length > 0 && (
            <button
              className="chat-jump"
              onClick={() => {
                follow.current = true;
                viewport.current?.scrollTo({
                  top: viewport.current.scrollHeight,
                  behavior: "smooth",
                });
              }}
            >
              <ArrowDown size={13} />
              Latest response
            </button>
          )}
          <form
            className="chat-composer"
            onSubmit={(e) => {
              e.preventDefault();
              submit();
            }}
          >
            <textarea
              ref={composer}
              aria-label="Message"
              placeholder={
                session.models.length
                  ? "Message your models…"
                  : "Write a message, then choose a model…"
              }
              value={session.draft}
              rows={3}
              onChange={(e) => draft(e.target.value)}
              onKeyDown={(e) => {
                if (
                  e.key === "Enter" &&
                  !e.shiftKey &&
                  !e.nativeEvent.isComposing
                ) {
                  e.preventDefault();
                  submit();
                }
              }}
            />
            <div className="chat-composer-bottom">
              <div className="composer-tools">
                <button
                  type="button"
                  className="icon-button"
                  aria-label="Attach text file"
                  title="Add a text file to the draft"
                  onClick={() => fileInput.current?.click()}
                >
                  <Paperclip size={16} />
                </button>
                <GenerationSettings />
                <span className="composer-mode">
                  <span className="footer-dot" />
                  {mode === "demo"
                    ? "Demo · no inference"
                    : `Send to ${session.models.length} model${session.models.length === 1 ? "" : "s"}`}
                </span>
              </div>
              {busy ? (
                <button
                  type="button"
                  className="primary"
                  onClick={() => stop()}
                >
                  <Square size={13} />
                  {session.models.length > 1
                    ? "Stop all responses"
                    : "Stop response"}
                </button>
              ) : (
                <button
                  type="submit"
                  className="primary chat-send"
                  aria-label="Send message"
                  disabled={!canSend}
                >
                  <ArrowUp size={16} />
                  Send
                  {session.models.length > 1
                    ? ` to ${session.models.length}`
                    : ""}
                </button>
              )}
            </div>
          </form>
          <input
            ref={fileInput}
            className="sr-only"
            type="file"
            aria-label="Attach plain text file"
            accept=".txt,.md,.json,.csv,.log,.ts,.js,.py,.rs,.html,.css,.yaml,.yml"
            onChange={(e) => {
              void attach(e.target.files?.[0]);
              e.target.value = "";
            }}
          />
          <div className="chat-compose-hint">
            <span>
              {session.settings.system.trim() ? "System prompt active · " : ""}
              Enter to send · Shift + Enter for a new line
            </span>
            <span role="status">
              {busy
                ? activeStream === session.id
                  ? "Streaming responses…"
                  : "A response is streaming in another conversation."
                : mode === "demo"
                  ? "Fixture responses"
                  : "Uses your configured providers"}
            </span>
          </div>
        </div>
      </section>
      {reasoning && (
        <ReasoningSidebar
          message={reasoning}
          close={() => setReasoningId(undefined)}
        />
      )}
    </div>
  );
}
