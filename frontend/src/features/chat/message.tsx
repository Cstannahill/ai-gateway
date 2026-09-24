"use client";
import { useEffect, useRef, useState } from "react";
import Link from "next/link";
import * as Dialog from "@radix-ui/react-dialog";
import {
  ArrowDown,
  ArrowUp,
  Bot,
  ChevronDown,
  Copy,
  Download,
  ExternalLink,
  Maximize2,
  Minimize2,
  MoreHorizontal,
  Pencil,
  RotateCcw,
  Sparkles,
  Square,
  Trash2,
  User,
  X,
} from "lucide-react";
import { useRuntime } from "@/components/runtime";
import { tokenCount } from "@/lib/chat-session";
import { useChat, type ChatMessage } from "./state";
import { Modal, SafeMarkdown, downloadChat } from "./shared";
export function Message({
  message,
  onReasoning,
  reasoningOpen,
}: {
  message: ChatMessage;
  onReasoning: (id: string) => void;
  reasoningOpen: boolean;
}) {
  const { busy, send, stop, display, editMessage, deleteMessage } = useChat(),
    { mode } = useRuntime();
  const [copied, setCopied] = useState(""),
    [expanded, setExpanded] = useState(false),
    [editing, setEditing] = useState(false),
    [text, setText] = useState(message.content);
  const root = useRef<HTMLElement>(null),
    content = useRef<HTMLDivElement>(null),
    following = useRef(true);
  const assistant = message.role === "assistant",
    streaming = message.status === "streaming",
    usage = message.usage;
  useEffect(() => {
    if (streaming && following.current && content.current)
      content.current.scrollTop = content.current.scrollHeight;
  }, [message.content, streaming]);
  async function copy() {
    try {
      await navigator.clipboard.writeText(message.content);
      setCopied("Copied");
    } catch {
      setCopied("Copy unavailable; use export");
    }
  }
  const tokens = (value: unknown) =>
    typeof value === "number" && Number.isSafeInteger(value) && value >= 0
      ? value.toLocaleString()
      : "—";
  return (
    <article
      ref={root}
      className={
        "chat-message " + (assistant ? "assistant response-card" : "user")
      }
      aria-label={assistant ? "Assistant response" : "Your message"}
      data-model={message.requestedModel}
    >
      <div className="response-heading">
        <span className="chat-avatar">
          {assistant ? <Bot size={16} /> : <User size={16} />}
        </span>
        <strong>{assistant ? message.requestedModel : "You"}</strong>
        <span className="response-status">
          {streaming
            ? "Generating…"
            : message.status === "stopped"
              ? "Stopped · partial"
              : message.status === "error"
                ? "Failed · retained"
                : message.edited
                  ? "Edited"
                  : message.imported
                    ? "Imported"
                    : ""}
        </span>
        {assistant && (
          <button
            className="icon-button"
            aria-label={expanded ? "Collapse response" : "Expand response"}
            onClick={() => setExpanded(!expanded)}
          >
            {expanded ? <Minimize2 size={14} /> : <Maximize2 size={14} />}
          </button>
        )}
      </div>
      {assistant && message.reasoning && (
        <button
          className={"reasoning-toggle " + (reasoningOpen ? "selected" : "")}
          onClick={() => onReasoning(message.id)}
          aria-pressed={reasoningOpen}
        >
          <Sparkles size={13} />
          Reasoning{streaming && <span className="chat-pulse" />}
        </button>
      )}
      <div
        ref={content}
        className={
          "response-content " + (expanded || display.expand ? "expanded" : "")
        }
        onScroll={() => {
          const v = content.current;
          if (v)
            following.current =
              v.scrollHeight - v.scrollTop - v.clientHeight < 65;
        }}
      >
        {message.content ? (
          assistant ? (
            <SafeMarkdown>{message.content}</SafeMarkdown>
          ) : (
            <p className="chat-user-text">{message.content}</p>
          )
        ) : streaming ? (
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
      </div>
      {assistant && (
        <button
          className="response-bottom"
          aria-label="Scroll response to bottom"
          onClick={() =>
            content.current?.scrollTo({
              top: content.current.scrollHeight,
              behavior: "smooth",
            })
          }
        >
          <ChevronDown size={14} />
        </button>
      )}
      <div className="response-footer">
        <div className="chat-message-actions">
          {assistant &&
            (streaming ? (
              <button
                aria-label={`Stop ${message.requestedModel}`}
                onClick={() => stop(message.id)}
                title="Stop this model"
              >
                <Square size={13} />
              </button>
            ) : (
              <button
                disabled={busy}
                onClick={() => void send(message.id)}
                aria-label="Regenerate in new chat"
                title="Regenerate this model in a new chat"
              >
                <RotateCcw size={13} />
              </button>
            ))}
          <button
            onClick={() => void copy()}
            disabled={!message.content}
            aria-label={assistant ? "Copy response" : "Copy message"}
            title={copied || "Copy Markdown"}
          >
            <Copy size={13} />
          </button>
          <button
            disabled={busy}
            aria-label={assistant ? "Edit response" : "Edit message"}
            title="Edit in a new conversation branch"
            onClick={() => {
              setText(message.content);
              setEditing(true);
            }}
          >
            <Pencil size={13} />
          </button>
          <button
            disabled={busy}
            aria-label={assistant ? "Delete response" : "Delete turn"}
            title={
              assistant
                ? "Remove this answer from context (undo available)"
                : "Remove this prompt and all its answers (undo available)"
            }
            onClick={() => deleteMessage(message.id)}
          >
            <Trash2 size={13} />
          </button>
          <Modal
            title="Response evidence"
            description="Inspect or export the original request and observed response."
            trigger={
              <button aria-label="More message actions" title="More actions">
                <MoreHorizontal size={14} />
              </button>
            }
          >
            <div className="chat-evidence">
              <button
                onClick={() =>
                  downloadChat(
                    `message-${message.id}.json`,
                    JSON.stringify(message, null, 2),
                  )
                }
              >
                <Download size={14} />
                Export message JSON
              </button>
              {assistant && (
                <div className="chat-response-meta">
                  {message.elapsedMs !== undefined && (
                    <span>
                      {(message.elapsedMs / 1000).toFixed(1)}s browser elapsed
                    </span>
                  )}
                  {message.finishReason && (
                    <span>Finish: {message.finishReason}</span>
                  )}
                  {message.resolvedModel && (
                    <span>Resolved: {message.resolvedModel}</span>
                  )}
                </div>
              )}
              <h3>Usage observation</h3>
              <p className="chat-hint">
                Latest cumulative observation, never summed across updates.
                Gateway zeros can also mean unavailable. Per-response prices are
                not currently exposed. Demo prices are fixtures.
              </p>
              {usage ? (
                <>
                  <p>
                    {tokens(usage.prompt_tokens)} input ·{" "}
                    {tokens(usage.completion_tokens)} output tokens*
                  </p>
                  <pre>{JSON.stringify(usage, null, 2)}</pre>
                </>
              ) : (
                <p>No usage observation received.</p>
              )}
              {message.sent && (
                <details>
                  <summary>Request payload</summary>
                  <pre>{JSON.stringify(message.sent, null, 2)}</pre>
                </details>
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
          </Modal>
          <button
            aria-label="Jump to message start"
            title="Jump to message start"
            onClick={() =>
              root.current?.scrollIntoView({
                block: "start",
                behavior: "smooth",
              })
            }
          >
            <ArrowUp size={13} />
          </button>
          {message.requestId && mode === "live" && (
            <Link
              className="button inspect-chat-request"
              title={message.requestId}
              href={`/?view=chat&request=${message.requestId}`}
            >
              <ExternalLink size={13} />
              <span>Inspect request</span>
            </Link>
          )}
        </div>
        {assistant && display.metrics && (
          <span
            className="response-price"
            title={
              mode === "demo" && message.demoCost !== undefined
                ? "Demo fixture price and token counts. Not a bill."
                : "Per-response cost is not exposed by the gateway. Tokens are the latest observation; zero may represent unavailable."
            }
          >
            {mode === "demo" && message.demoCost !== undefined
              ? `$${message.demoCost.toFixed(5).replace(/0+$/, "").replace(/\.$/, ".00")} demo`
              : "— cost"}{" "}
            <span>·</span> {tokenCount(usage)?.toLocaleString() ?? "—"} tok
            {message.imported ? " (imported)" : ""}
          </span>
        )}
      </div>
      {copied && (
        <span className="chat-copy-status" role="status">
          {copied}
        </span>
      )}
      <Dialog.Root open={editing} onOpenChange={setEditing}>
        <Dialog.Portal>
          <Dialog.Overlay className="drawer-overlay" />
          <Dialog.Content className="chat-modal">
            <div className="chat-modal-heading">
              <div>
                <Dialog.Title>
                  Edit {assistant ? "response" : "message"}
                </Dialog.Title>
                <Dialog.Description>
                  Save into a new branch; the original stays intact. Editing a
                  prompt places it in the composer for review. No inference is
                  sent.
                </Dialog.Description>
              </div>
              <Dialog.Close className="icon-button" aria-label="Close editor">
                <X size={17} />
              </Dialog.Close>
            </div>
            <textarea
              className="chat-edit-text"
              aria-label="Edited message"
              rows={12}
              value={text}
              onChange={(e) => setText(e.target.value)}
            />
            <button
              className="primary"
              disabled={busy || !text.trim()}
              onClick={() => {
                editMessage(message.id, text);
                setEditing(false);
              }}
            >
              Save edited branch
            </button>
          </Dialog.Content>
        </Dialog.Portal>
      </Dialog.Root>
    </article>
  );
}
export function ReasoningSidebar({
  message,
  close,
}: {
  message: ChatMessage;
  close: () => void;
}) {
  const { mode } = useRuntime();
  const viewport = useRef<HTMLDivElement>(null),
    follow = useRef(true);
  useEffect(() => {
    if (follow.current && viewport.current)
      viewport.current.scrollTop = viewport.current.scrollHeight;
  }, [message.reasoning]);
  useEffect(() => {
    const key = (e: KeyboardEvent) => {
      if (e.key === "Escape") close();
    };
    window.addEventListener("keydown", key);
    return () => window.removeEventListener("keydown", key);
  }, [close]);
  return (
    <aside className="reasoning-sidebar" aria-label="Reasoning sidebar">
      <header>
        <div>
          <strong>{message.requestedModel}</strong>
          <small>
            <Sparkles size={12} />
            Reasoning ·{" "}
            {mode === "demo"
              ? "demo fixture"
              : message.imported
                ? "imported text"
                : message.status === "streaming"
                  ? "streaming"
                  : "provider output"}
          </small>
        </div>
        <button
          className="icon-button"
          aria-label="Close reasoning sidebar"
          onClick={close}
        >
          <X size={16} />
        </button>
      </header>
      <div
        className="reasoning-scroll"
        ref={viewport}
        onScroll={() => {
          const v = viewport.current;
          if (v)
            follow.current = v.scrollHeight - v.scrollTop - v.clientHeight < 60;
        }}
      >
        <SafeMarkdown>
          {message.reasoning ||
            "No reasoning text has been returned for this response."}
        </SafeMarkdown>
      </div>
      <footer>
        <button
          onClick={() =>
            viewport.current?.scrollTo({
              top: viewport.current.scrollHeight,
              behavior: "smooth",
            })
          }
        >
          <ArrowDown size={13} />
          Latest reasoning
        </button>
      </footer>
    </aside>
  );
}
