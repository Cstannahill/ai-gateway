"use client";
import { useEffect, useRef, useState, type ReactNode } from "react";
import Link from "next/link";
import * as Dialog from "@radix-ui/react-dialog";
import Markdown from "react-markdown";
import remarkGfm from "remark-gfm";
import {
  ArrowDown,
  ArrowUp,
  Bot,
  Check,
  ChevronDown,
  Code2,
  Copy,
  Download,
  ExternalLink,
  History,
  MessageSquare,
  Plus,
  RotateCcw,
  Search,
  SlidersHorizontal,
  Sparkles,
  Square,
  Trash2,
  User,
  X,
} from "lucide-react";
import { useRead, useRuntime } from "@/components/runtime";
import type { Model } from "@/lib/contracts";
import { useChat, type ChatMessage } from "./state";

function Modal({
  title,
  description,
  children,
  trigger,
}: {
  title: string;
  description: string;
  children: ReactNode;
  trigger: ReactNode;
}) {
  return (
    <Dialog.Root>
      <Dialog.Trigger asChild>{trigger}</Dialog.Trigger>
      <Dialog.Portal>
        <Dialog.Overlay className="drawer-overlay" />
        <Dialog.Content className="chat-modal">
          <div className="chat-modal-heading">
            <div>
              <Dialog.Title>{title}</Dialog.Title>
              <Dialog.Description>{description}</Dialog.Description>
            </div>
            <Dialog.Close className="icon-button" aria-label="Close dialog">
              <X size={18} />
            </Dialog.Close>
          </div>
          {children}
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
function ModelPicker() {
  const { session, configure, busy } = useChat();
  const catalog = useRead<{ data: Model[] }>("/v1/models", 120000);
  const [open, setOpen] = useState(false),
    [query, setQuery] = useState(""),
    [custom, setCustom] = useState("");
  const filtered = (catalog.data?.data ?? []).filter((m) =>
    (m.id + " " + m.providers.join(" "))
      .toLowerCase()
      .includes(query.toLowerCase()),
  );
  function choose(model: string) {
    configure({ model, effort: "" });
    setOpen(false);
  }
  return (
    <Dialog.Root open={open} onOpenChange={setOpen}>
      <Dialog.Trigger asChild>
        <button className="chat-model-trigger" disabled={busy}>
          <span className="chat-model-symbol">
            <Bot size={19} />
          </span>
          <span>
            <small>MODEL / GATEWAY ALIAS</small>
            <strong>{session.settings.model || "Choose a model"}</strong>
          </span>
          <ChevronDown size={15} />
        </button>
      </Dialog.Trigger>
      <Dialog.Portal>
        <Dialog.Overlay className="drawer-overlay" />
        <Dialog.Content className="chat-modal">
          <div className="chat-modal-heading">
            <div>
              <Dialog.Title>Choose a model</Dialog.Title>
              <Dialog.Description>
                Select a discovered model or enter a configured gateway alias.
              </Dialog.Description>
            </div>
            <Dialog.Close
              className="icon-button"
              aria-label="Close model picker"
            >
              <X size={18} />
            </Dialog.Close>
          </div>
          <label className="chat-search">
            <Search size={16} />
            <input
              aria-label="Search chat models"
              placeholder="Search models and providers…"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
            />
          </label>
          {catalog.isPending ? (
            <p className="chat-hint">Loading gateway models…</p>
          ) : catalog.isError ? (
            <div className="chat-catalog-error" role="status">
              <p>{catalog.error.message}</p>
              <button onClick={() => catalog.refetch()}>
                Retry model discovery
              </button>
            </div>
          ) : null}
          <div className="chat-model-list">
            {filtered.slice(0, 100).map((m) => (
              <button
                key={m.id}
                onClick={() => choose(m.id)}
                disabled={!m.capabilities.streaming}
                title={
                  !m.capabilities.streaming
                    ? "This model does not advertise streaming support."
                    : m.id
                }
              >
                <Bot size={18} />
                <span>
                  <strong>{m.id}</strong>
                  <small>
                    {m.providers.join(" · ")}
                    {m.capabilities.context_window
                      ? ` · ${m.capabilities.context_window.toLocaleString()} context`
                      : ""}
                    {m.capabilities.reasoning ? " · reasoning" : ""}
                  </small>
                </span>
                {m.id === session.settings.model && <Check size={16} />}
              </button>
            ))}
            {catalog.data && !filtered.length && (
              <p className="chat-hint">
                No matching models. You can enter an explicit alias below.
              </p>
            )}
            {filtered.length > 100 && (
              <p className="chat-hint">
                Showing 100 matches. Narrow your search.
              </p>
            )}
          </div>
          <form
            className="chat-custom-model"
            onSubmit={(e) => {
              e.preventDefault();
              if (custom.trim()) choose(custom.trim());
            }}
          >
            <label htmlFor="custom-model">Model ID or gateway alias</label>
            <div>
              <input
                id="custom-model"
                placeholder="e.g. coding-fast"
                maxLength={512}
                value={custom}
                onChange={(e) => setCustom(e.target.value)}
              />
              <button type="submit" disabled={!custom.trim()}>
                Use model
              </button>
            </div>
            <small>
              The gateway validates availability and routes your selection.
            </small>
          </form>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
function Settings() {
  const { session, configure, busy } = useChat(),
    s = session.settings;
  return (
    <Modal
      title="Generation settings"
      description="These settings apply to the next response in this conversation."
      trigger={
        <button
          className="icon-button"
          aria-label="Generation settings"
          disabled={busy}
        >
          <SlidersHorizontal size={17} />
        </button>
      }
    >
      <div className="chat-settings">
        <label>
          System prompt
          <textarea
            rows={5}
            value={s.system}
            onChange={(e) => configure({ system: e.target.value })}
            placeholder="Instructions for this conversation…"
          />
        </label>
        <div className="chat-setting-grid">
          <label>
            Temperature
            <input
              type="number"
              min={0}
              max={2}
              step="0.1"
              value={s.temperature}
              placeholder="Provider default"
              onChange={(e) => configure({ temperature: e.target.value })}
            />
          </label>
          <label>
            Max completion tokens
            <input
              type="number"
              min={1}
              step="1"
              value={s.maxTokens}
              placeholder="Provider default"
              onChange={(e) => configure({ maxTokens: e.target.value })}
            />
          </label>
        </div>
        <label>
          Reasoning effort
          <select
            value={s.effort}
            onChange={(e) =>
              configure({ effort: e.target.value as typeof s.effort })
            }
          >
            <option value="">Provider default</option>
            <option value="low">Low</option>
            <option value="medium">Medium</option>
            <option value="high">High</option>
          </select>
        </label>
        <p className="chat-hint">
          Blank values leave the provider defaults in place. Parameter support
          depends on the selected model. Unsupported parameters are reported by
          the gateway; they are never retried automatically.
        </p>
      </div>
    </Modal>
  );
}
function SafeMarkdown({ children }: { children: string }) {
  return (
    <div className="chat-markdown">
      <Markdown
        remarkPlugins={[remarkGfm]}
        skipHtml
        urlTransform={(url) => (/^https?:\/\//i.test(url) ? url : "")}
        components={{
          img: ({ alt }) => (
            <span className="muted">[Image: {alt || "not loaded"}]</span>
          ),
          a: ({ href, children }) =>
            href ? (
              <a
                href={href}
                target="_blank"
                rel="noopener noreferrer"
                referrerPolicy="no-referrer"
              >
                {children}
              </a>
            ) : (
              <span>{children}</span>
            ),
        }}
      >
        {children}
      </Markdown>
    </div>
  );
}
function Message({ message, last }: { message: ChatMessage; last: boolean }) {
  const { busy, send } = useChat(),
    { mode } = useRuntime();
  const [copied, setCopied] = useState("");
  const assistant = message.role === "assistant";
  async function copy() {
    try {
      await navigator.clipboard.writeText(message.content);
      setCopied("Copied");
    } catch {
      setCopied("Copy unavailable; use export");
    }
  }
  const usage = message.usage;
  const tokens = (value: unknown) =>
    typeof value === "number" && Number.isSafeInteger(value) && value >= 0
      ? value.toLocaleString()
      : "—";
  return (
    <article
      className={"chat-message " + (assistant ? "assistant" : "user")}
      aria-label={assistant ? "Assistant response" : "Your message"}
    >
      <span className="chat-avatar">
        {assistant ? <Bot size={18} /> : <User size={17} />}
      </span>
      <div className="chat-message-body">
        <div className="chat-message-heading">
          <strong>{assistant ? message.requestedModel : "You"}</strong>
          <span>
            {message.status === "streaming"
              ? "Generating…"
              : message.status === "stopped"
                ? "Stopped · partial"
                : message.status === "error"
                  ? "Failed · retained"
                  : assistant
                    ? "Response"
                    : ""}
          </span>
        </div>
        {message.reasoning && (
          <details className="chat-reasoning">
            <summary>
              Reasoning{" "}
              <span>
                {message.status === "streaming"
                  ? "streaming"
                  : "provider output"}
              </span>
            </summary>
            <pre>{message.reasoning}</pre>
          </details>
        )}
        {message.content ? (
          assistant ? (
            <SafeMarkdown>{message.content}</SafeMarkdown>
          ) : (
            <p className="chat-user-text">{message.content}</p>
          )
        ) : message.status === "streaming" ? (
          <p className="chat-wait">
            <span className="chat-pulse" />
            Waiting for the model…
          </p>
        ) : (
          <p className="chat-hint">No response text received.</p>
        )}
        {message.error && (
          <div className="chat-error" role="alert">
            {message.error}
          </div>
        )}
        {assistant && (
          <>
            <div className="chat-response-meta">
              {message.resolvedModel &&
                message.resolvedModel !== message.requestedModel && (
                  <span title="Resolved model">{message.resolvedModel}</span>
                )}
              {message.elapsedMs !== undefined && (
                <span>
                  {(message.elapsedMs / 1000).toFixed(1)}s browser elapsed
                </span>
              )}
              {message.finishReason && (
                <span>Finish: {message.finishReason}</span>
              )}
              {usage && (
                <span title="Latest gateway usage observation. Cumulative updates are not summed; gateway zeros may also represent unavailable counts.">
                  {tokens(usage.prompt_tokens)} input ·{" "}
                  {tokens(usage.completion_tokens)} output tokens*
                </span>
              )}
            </div>
            {usage && (
              <details className="chat-usage">
                <summary>Usage observation*</summary>
                <p>
                  Latest reported counts; gateway zero values may also mean
                  unavailable. These are observations, not billing estimates.
                </p>
                <pre>{JSON.stringify(usage, null, 2)}</pre>
              </details>
            )}
            <div className="chat-message-actions">
              <button
                onClick={copy}
                disabled={!message.content}
                aria-label="Copy response"
              >
                <Copy size={13} />
                {copied || "Copy"}
              </button>
              {last && message.status !== "streaming" && (
                <button disabled={busy} onClick={() => void send(true)}>
                  <RotateCcw size={13} />
                  Regenerate in new chat
                </button>
              )}
              {message.requestId && mode === "live" && (
                <Link
                  className="button"
                  title={message.requestId}
                  href={`/?view=chat&request=${message.requestId}`}
                >
                  <ExternalLink size={13} />
                  Inspect request
                </Link>
              )}
            </div>
          </>
        )}
      </div>
    </article>
  );
}
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
  } = useChat();
  const { mode } = useRuntime();
  const [history, setHistory] = useState(false),
    [search, setSearch] = useState(""),
    [atBottom, setAtBottom] = useState(true);
  const viewport = useRef<HTMLDivElement>(null),
    composer = useRef<HTMLTextAreaElement>(null),
    follow = useRef(true);
  const messages = session.messages;
  useEffect(() => {
    if (follow.current && viewport.current)
      viewport.current.scrollTop = viewport.current.scrollHeight;
  }, [messages]);
  useEffect(() => {
    follow.current = true;
    if (viewport.current)
      viewport.current.scrollTop = viewport.current.scrollHeight;
  }, [session.id]);
  function exportChat() {
    const url = URL.createObjectURL(
      new Blob(
        [
          JSON.stringify(
            {
              format: "ai-gateway-chat-v1",
              mode,
              exportedAt: new Date().toISOString(),
              ...session,
            },
            null,
            2,
          ),
        ],
        { type: "application/json" },
      ),
    );
    const a = document.createElement("a");
    a.href = url;
    a.download = `chat-${session.id}.json`;
    a.click();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
  }
  return (
    <div className="chat-workspace">
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
        <span className="nav-label">THIS SESSION</span>
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
            <Settings />
            <button
              className="icon-button"
              aria-label="Export conversation"
              onClick={exportChat}
              disabled={!messages.length}
            >
              <Download size={17} />
            </button>
          </div>
        </div>
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
                Choose a model, start a conversation,
                <br />
                and follow every response back to its evidence.
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
            <div className="chat-messages">
              {messages.map((m, i) => (
                <Message
                  key={m.id}
                  message={m}
                  last={i === messages.length - 1}
                />
              ))}
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
              follow.current = true;
              void send();
            }}
          >
            <textarea
              ref={composer}
              aria-label="Message"
              placeholder={
                session.settings.model
                  ? "Message your model…"
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
                  if (!busy && session.settings.model && session.draft.trim()) {
                    follow.current = true;
                    void send();
                  }
                }
              }}
            />
            <div className="chat-composer-bottom">
              <span>
                <span
                  className={"footer-dot " + (mode === "demo" ? "demo" : "")}
                />
                {mode === "demo"
                  ? "Demo · no inference requests"
                  : "Text chat · via AI Gateway"}
              </span>
              {busy ? (
                <button type="button" className="primary" onClick={stop}>
                  <Square size={13} />
                  Stop response
                </button>
              ) : (
                <button
                  type="submit"
                  className="primary chat-send"
                  aria-label="Send message"
                  disabled={!session.draft.trim() || !session.settings.model}
                >
                  <ArrowUp size={16} />
                  Send
                </button>
              )}
            </div>
          </form>
          <div className="chat-compose-hint">
            <span>
              {session.settings.system.trim() ? "System prompt active · " : ""}
              Enter to send · Shift + Enter for a new line
            </span>
            <span role="status">
              {busy
                ? activeStream === session.id
                  ? "Streaming response…"
                  : "A response is streaming in another conversation."
                : mode === "demo"
                  ? "Fixture responses"
                  : "Uses your configured providers"}
            </span>
          </div>
        </div>
      </section>
    </div>
  );
}
