"use client";
import { useRef, useState } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import {
  Copy,
  Download,
  FileText,
  FolderOpen,
  Palette,
  Settings2,
  Share2,
  SlidersHorizontal,
  Trash2,
  Upload,
  UserRound,
  X,
} from "lucide-react";
import { useRuntime } from "@/components/runtime";
import { importChat, markdownExport, modelSelection } from "@/lib/chat-session";
import { useChat } from "./state";
import { Modal, downloadChat } from "./shared";
export function GenerationSettings() {
  const { session, configure, busy } = useChat(),
    s = session.settings;
  return (
    <Modal
      title="Generation settings"
      description="Shared settings apply to the next response from every selected model."
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
          Blank values use provider defaults. Each selected model receives the
          shared user turns and only its own completed answers. Parameter
          support is model-specific.
        </p>
      </div>
    </Modal>
  );
}
export function RoomControls({
  onNotice,
}: {
  onNotice: (text: string) => void;
}) {
  const {
    session,
    busy,
    display,
    setDisplay,
    duplicate,
    clear,
    models,
    importSession,
    configure,
    rename,
    removeFile,
  } = useChat();
  const { mode } = useRuntime();
  const menu = useRef<HTMLDetailsElement>(null),
    input = useRef<HTMLInputElement>(null),
    characterInput = useRef<HTMLInputElement>(null);
  const [character, setCharacter] = useState<{
    name: string;
    prompt: string;
  } | null>(null);
  const close = () => {
    if (menu.current) menu.current.open = false;
  };
  async function read(file?: File, isCharacter = false) {
    if (!file) return;
    try {
      if (file.size > 8 * 1024 * 1024)
        throw new Error("File exceeds the 8 MiB import limit.");
      const raw = await file.text();
      if (isCharacter) {
        const json = JSON.parse(raw),
          data = json.data ?? json;
        if (!data || typeof data !== "object")
          throw new Error("Invalid character JSON.");
        const fields = [
          "system_prompt",
          "description",
          "personality",
          "scenario",
        ];
        const prompt = fields
          .filter((k) => typeof data[k] === "string" && data[k].trim())
          .map((k) => `${k.replaceAll("_", " ")}:\n${data[k]}`)
          .join("\n\n");
        if (!prompt)
          throw new Error(
            "Character JSON must contain a system_prompt, description, personality or scenario.",
          );
        setCharacter({
          name: typeof data.name === "string" ? data.name : file.name,
          prompt,
        });
      } else {
        const json = JSON.parse(raw);
        if (json.format === "ai-gateway-models-v1") {
          models(modelSelection(json.models));
          onNotice("Model selection imported. No inference sent.");
        } else {
          importSession(importChat(raw));
          onNotice("Conversation imported into this mode. No inference sent.");
        }
      }
    } catch (e) {
      onNotice(e instanceof Error ? e.message : "Import failed.");
    }
  }
  async function share() {
    try {
      await navigator.clipboard.writeText(
        JSON.stringify(
          { format: "ai-gateway-models-v1", models: session.models },
          null,
          2,
        ),
      );
      onNotice(
        "Model selection copied as JSON. No conversation content included.",
      );
    } catch {
      onNotice("Clipboard unavailable. Use Export models instead.");
    }
    close();
  }
  return (
    <>
      <details className="chat-room-menu" ref={menu}>
        <summary aria-label="Chat controls" title="Chat controls">
          <Settings2 size={17} />
          <span className="sr-only">Chat controls</span>
        </summary>
        <div className="chat-room-popover">
          <label className="room-name">
            Conversation name
            <input
              aria-label="Conversation name"
              maxLength={120}
              value={session.title}
              onChange={(e) => rename(e.target.value)}
            />
          </label>
          <button
            onClick={() => {
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
              );
              close();
            }}
          >
            <Download size={15} />
            Export conversation
          </button>
          <button
            disabled={busy}
            onClick={() => {
              input.current?.click();
              close();
            }}
          >
            <Upload size={15} />
            Import conversation / models
          </button>
          <button
            onClick={() => {
              downloadChat(
                `chat-${session.id}.md`,
                markdownExport(session),
                "text/markdown",
              );
              close();
            }}
          >
            <FileText size={15} />
            Get Markdown
          </button>
          <button onClick={() => void share()}>
            <Share2 size={15} />
            Share models
          </button>
          <button
            onClick={() => {
              downloadChat(
                "gateway-models.json",
                JSON.stringify(
                  { format: "ai-gateway-models-v1", models: session.models },
                  null,
                  2,
                ),
              );
              close();
            }}
          >
            <Download size={15} />
            Export models
          </button>
          <button
            disabled={busy}
            onClick={() => {
              characterInput.current?.click();
              close();
            }}
          >
            <UserRound size={15} />
            Import character
          </button>
          <button
            disabled={busy}
            onClick={() => {
              duplicate();
              close();
            }}
          >
            <Copy size={15} />
            Duplicate room
          </button>
          <button
            disabled={busy || !session.models.length}
            onClick={() => {
              models([]);
              close();
            }}
          >
            <X size={15} />
            Clear models
          </button>
          <button
            disabled={busy || !session.messages.length}
            onClick={() => {
              clear();
              close();
            }}
          >
            <Trash2 size={15} />
            Clear chat
          </button>
          <div className="room-toggles">
            <label>
              Always expand messages
              <input
                type="checkbox"
                role="switch"
                checked={display.expand}
                onChange={(e) => setDisplay({ expand: e.target.checked })}
              />
            </label>
            <label>
              Show metrics inline
              <input
                type="checkbox"
                role="switch"
                checked={display.metrics}
                onChange={(e) => setDisplay({ metrics: e.target.checked })}
              />
            </label>
            <div className="room-colors">
              <Palette size={14} />
              <span>Message color</span>
              {[
                "gray",
                "blue",
                "teal",
                "amber",
                "orange",
                "pink",
                "purple",
              ].map((c) => (
                <button
                  key={c}
                  className={"color-dot " + c}
                  aria-label={`Use ${c} message color`}
                  aria-pressed={display.color === c}
                  onClick={() => setDisplay({ color: c })}
                />
              ))}
            </div>
          </div>
          <div className="room-files">
            <h3>
              <FolderOpen size={14} />
              Session files
            </h3>
            {session.files.length ? (
              session.files.map((f) => (
                <div key={f.name}>
                  <span>{f.name}</span>
                  <button
                    className="icon-button"
                    aria-label={`Remove file ${f.name}`}
                    onClick={() => removeFile(f.name)}
                  >
                    <X size={12} />
                  </button>
                </div>
              ))
            ) : (
              <p>No text files in this session yet.</p>
            )}
            <small>
              Attachments are inserted as text into the draft. Removing a file
              here does not erase sent messages.
            </small>
          </div>
        </div>
      </details>
      <input
        ref={input}
        className="sr-only"
        type="file"
        accept=".json,application/json"
        aria-label="Import chat JSON"
        onChange={(e) => {
          void read(e.target.files?.[0]);
          e.target.value = "";
        }}
      />
      <input
        ref={characterInput}
        className="sr-only"
        type="file"
        accept=".json,application/json"
        aria-label="Import character JSON"
        onChange={(e) => {
          void read(e.target.files?.[0], true);
          e.target.value = "";
        }}
      />
      <Dialog.Root
        open={!!character}
        onOpenChange={(open) => {
          if (!open) setCharacter(null);
        }}
      >
        <Dialog.Portal>
          <Dialog.Overlay className="drawer-overlay" />
          <Dialog.Content className="chat-modal">
            <div className="chat-modal-heading">
              <div>
                <Dialog.Title>Import character: {character?.name}</Dialog.Title>
                <Dialog.Description>
                  Review the system prompt before applying it to future turns.
                  No request is sent.
                </Dialog.Description>
              </div>
              <Dialog.Close
                className="icon-button"
                aria-label="Close character preview"
              >
                <X size={18} />
              </Dialog.Close>
            </div>
            <pre className="character-preview">{character?.prompt}</pre>
            <button
              className="primary"
              disabled={busy}
              onClick={() => {
                if (character) configure({ system: character.prompt });
                setCharacter(null);
                onNotice("Character applied as the system prompt.");
              }}
            >
              Apply character
            </button>
          </Dialog.Content>
        </Dialog.Portal>
      </Dialog.Root>
    </>
  );
}
