"use client";
import { useState } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import { Bot, Check, Filter, Pin, Plus, Search, X } from "lucide-react";
import { useRead } from "@/components/runtime";
import type { Model } from "@/lib/contracts";
import { MAX_MODELS } from "@/lib/chat-session";
import { useChat } from "./state";
export function ModelPicker() {
  const { session, models, busy, pins, recent, pin } = useChat();
  const catalog = useRead<{ data: Model[] }>("/v1/models", 120000);
  const [open, setOpen] = useState(false),
    [query, setQuery] = useState(""),
    [custom, setCustom] = useState(""),
    [provider, setProvider] = useState(""),
    [reasoning, setReasoning] = useState(false);
  const data = catalog.data?.data ?? [];
  const extra = [...new Set([...pins, ...recent])]
    .filter((id) => !data.some((m) => m.id === id))
    .map((id) => ({ id, providers: [] }) as Pick<Model, "id" | "providers">);
  const all = [...data, ...extra];
  const filtered = all.filter(
    (m) =>
      (m.id + " " + m.providers.join(" "))
        .toLowerCase()
        .includes(query.toLowerCase()) &&
      (!provider || m.providers.includes(provider)) &&
      (!reasoning ||
        ("capabilities" in m && (m as Model).capabilities.reasoning)),
  );
  const groups = [
    { title: "Pinned", items: filtered.filter((m) => pins.includes(m.id)) },
    {
      title: "Recent",
      items: filtered.filter(
        (m) => recent.includes(m.id) && !pins.includes(m.id),
      ),
    },
    {
      title: "All models",
      items: filtered.filter(
        (m) => !pins.includes(m.id) && !recent.includes(m.id),
      ),
    },
  ];
  function choose(id: string) {
    models(
      session.models.includes(id)
        ? session.models.filter((m) => m !== id)
        : [...session.models, id],
    );
    setOpen(false);
  }
  return (
    <Dialog.Root open={open} onOpenChange={setOpen}>
      <Dialog.Trigger asChild>
        <button
          className="chat-add-model"
          disabled={busy || session.models.length >= MAX_MODELS}
        >
          <Plus size={15} />
          {session.models.length ? "Add model" : "Choose a model"}
        </button>
      </Dialog.Trigger>
      <Dialog.Portal>
        <Dialog.Overlay className="drawer-overlay" />
        <Dialog.Content className="chat-modal model-picker-modal">
          <div className="chat-modal-heading">
            <div>
              <Dialog.Title>Choose a model</Dialog.Title>
              <Dialog.Description>
                {data.length} discovered models · up to {MAX_MODELS} responses
                per prompt
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
          <div className="chat-model-filters">
            <Filter size={14} />
            <select
              aria-label="Filter models by provider"
              value={provider}
              onChange={(e) => setProvider(e.target.value)}
            >
              <option value="">All providers</option>
              {[...new Set(data.flatMap((m) => m.providers))]
                .sort()
                .map((p) => (
                  <option key={p}>{p}</option>
                ))}
            </select>
            <label>
              <input
                type="checkbox"
                checked={reasoning}
                onChange={(e) => setReasoning(e.target.checked)}
              />
              Reasoning models
            </label>
          </div>
          {catalog.isPending && (
            <p className="chat-hint">Loading gateway models…</p>
          )}
          {catalog.isError && (
            <div className="chat-catalog-error" role="status">
              <p>{catalog.error.message}</p>
              <button onClick={() => catalog.refetch()}>
                Retry model discovery
              </button>
            </div>
          )}
          <div className="chat-model-list grouped-models">
            {groups.map(
              (group) =>
                group.items.length > 0 && (
                  <section key={group.title}>
                    <h3>
                      {group.title} <span>{group.items.length}</span>
                    </h3>
                    {group.items.slice(0, 100).map((m) => {
                      const cap =
                        "capabilities" in m
                          ? (m as Model).capabilities
                          : undefined;
                      return (
                        <div className="chat-model-option" key={m.id}>
                          <button
                            className="chat-model-choice"
                            onClick={() => choose(m.id)}
                            disabled={cap?.streaming === false}
                            title={
                              cap?.streaming === false
                                ? "Streaming is not advertised by this model."
                                : m.id
                            }
                          >
                            <Bot size={18} />
                            <span>
                              <strong>{m.id}</strong>
                              <small>
                                {m.providers.join(" · ") ||
                                  "Explicit ID · not in catalog"}
                                {cap?.context_window
                                  ? ` · ${cap.context_window.toLocaleString()} context`
                                  : ""}
                                {cap?.reasoning ? " · reasoning" : ""}
                              </small>
                            </span>
                            {session.models.includes(m.id) && (
                              <Check size={15} />
                            )}
                          </button>
                          <button
                            className={
                              "icon-button model-pin " +
                              (pins.includes(m.id) ? "pinned" : "")
                            }
                            aria-label={`${pins.includes(m.id) ? "Unpin" : "Pin"} model ${m.id}`}
                            aria-pressed={pins.includes(m.id)}
                            onClick={() => pin(m.id)}
                          >
                            <Pin size={14} />
                          </button>
                        </div>
                      );
                    })}
                    {group.items.length > 100 && (
                      <p className="chat-hint">
                        Showing 100 matches. Refine your search.
                      </p>
                    )}
                  </section>
                ),
            )}
            {catalog.data && !filtered.length && (
              <p className="chat-hint">No matching models.</p>
            )}
          </div>
          <form
            className="chat-custom-model"
            onSubmit={(e) => {
              e.preventDefault();
              if (
                custom.trim() &&
                (session.models.includes(custom.trim()) ||
                  session.models.length < MAX_MODELS)
              )
                choose(custom.trim());
            }}
          >
            <label htmlFor="custom-model">Model ID or gateway alias</label>
            <div>
              <input
                id="custom-model"
                maxLength={512}
                placeholder="e.g. coding-fast"
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
