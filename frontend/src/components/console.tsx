"use client";
import { useEffect, useState, type ReactNode } from "react";
import Link from "next/link";
import { useSearchParams, useRouter } from "next/navigation";
import { useQueryClient } from "@tanstack/react-query";
import {
  Activity,
  ArrowLeft,
  ArrowRight,
  Boxes,
  ChartNoAxesCombined,
  ChevronRight,
  FlaskConical,
  GitBranch,
  LayoutDashboard,
  Menu,
  MessageSquare,
  Moon,
  Pause,
  Play,
  RefreshCw,
  Search,
  Sun,
  X,
} from "lucide-react";
import { useRuntime } from "./runtime";
import { Overview } from "@/features/overview";
import { Explorer } from "@/features/requests/explorer";
import { RequestDrawer, DetailBody } from "@/features/requests/detail";
import { ComparisonView } from "@/features/comparison";
import { Reliability } from "@/features/reliability";
import { Inventory } from "@/features/inventory";
import { Chat } from "@/features/chat/chat";
const sections = [
  {
    key: "chat",
    title: "Chat",
    icon: MessageSquare,
    description:
      "A conversation with your models. A window into every response.",
  },
  {
    key: "overview",
    title: "Overview",
    icon: LayoutDashboard,
    description: "Your inference operations, in focus.",
  },
  {
    key: "requests",
    title: "Requests",
    icon: Search,
    description: "Trace a request from intent to outcome.",
  },
  {
    key: "comparison",
    title: "Comparison",
    icon: ChartNoAxesCombined,
    description: "Compare the performance you have observed.",
  },
  {
    key: "reliability",
    title: "Reliability",
    icon: GitBranch,
    description: "Understand what failed, and what recovered.",
  },
  {
    key: "inventory",
    title: "Inventory",
    icon: Boxes,
    description: "Provider instances and the models they expose.",
  },
];
function Shell({
  children,
  detail = false,
}: {
  children: ReactNode;
  detail?: boolean;
}) {
  const params = useSearchParams(),
    router = useRouter(),
    client = useQueryClient();
  const { mode, paused, setPaused, zone, setZone } = useRuntime();
  const [light, setLight] = useState(false),
    [mobile, setMobile] = useState(false);
  const current =
    sections.find((s) => s.key === params.get("view")) ?? sections[1];
  useEffect(() => {
    // Hydrate a harmless persisted preference after the server render.
    const saved = localStorage.getItem("gateway-theme");
    if (saved === "light") {
      document.documentElement.dataset.theme = "light";
      // eslint-disable-next-line react-hooks/set-state-in-effect
      setLight(true);
    }
  }, []);
  const href = (view: string) =>
    "/?" +
    new URLSearchParams({ view, ...(mode === "demo" ? { demo: "1" } : {}) });
  const returnParams = new URLSearchParams(
    params.get("back") ?? "view=requests",
  );
  returnParams.delete("request");
  if (mode === "demo") returnParams.set("demo", "1");
  else returnParams.delete("demo");
  const returnHref = "/?" + returnParams;
  function toggleTheme() {
    const next = !light;
    setLight(next);
    document.documentElement.dataset.theme = next ? "light" : "dark";
    localStorage.setItem("gateway-theme", next ? "light" : "dark");
  }
  function toggleMode() {
    const p = new URLSearchParams(params);
    if (mode === "demo") p.delete("demo");
    else p.set("demo", "1");
    p.delete("request");
    router.push("/?" + p);
  }
  return (
    <div className="app-shell">
      <a className="skip-link" href="#main">
        Skip to content
      </a>
      {mobile && (
        <button
          className="mobile-scrim"
          aria-label="Close navigation"
          onClick={() => setMobile(false)}
        />
      )}
      <aside className={"sidebar " + (mobile ? "mobile-open" : "")}>
        <Link className="brand" href={href("overview")}>
          <span className="brand-symbol">
            <Activity size={21} />
          </span>
          <span>
            AI Gateway<small>OBSERVABILITY</small>
          </span>
        </Link>
        <div className="workspace">
          <span className="workspace-icon">P</span>
          <div>
            Personal workspace<small>Inference console</small>
          </div>
          <span className="workspace-dot" />
        </div>
        <span className="nav-label">WORKSPACE</span>
        <nav aria-label="Primary navigation">
          {sections.map((s) => (
            <Link
              key={s.key}
              href={href(s.key)}
              onClick={() => setMobile(false)}
              className={current.key === s.key && !detail ? "active" : ""}
              aria-current={
                current.key === s.key && !detail ? "page" : undefined
              }
            >
              <s.icon size={17} />
              {s.title}
              {current.key === s.key && !detail && (
                <span className="nav-marker" />
              )}
            </Link>
          ))}
        </nav>
        <div className="sidebar-bottom">
          <div className="observation-note">
            <Activity size={17} />
            <p>
              Store observations.
              <br />
              <strong>Calculate summaries.</strong>
            </p>
          </div>
          <button onClick={toggleMode}>
            <FlaskConical size={16} />
            {mode === "demo" ? "Return to live data" : "Explore demo data"}
            <ArrowRight size={13} />
          </button>
          <small>
            Inference console <span>v0.2</span>
          </small>
        </div>
      </aside>
      <div className="main-shell">
        <header className="topbar">
          <div className="breadcrumb">
            <button
              className="mobile-menu icon-button"
              aria-label="Open navigation"
              onClick={() => setMobile(!mobile)}
            >
              {mobile ? <X size={18} /> : <Menu size={18} />}
            </button>
            <span>Workspace</span>
            <ChevronRight size={13} />
            <strong>{detail ? "Request investigation" : current.title}</strong>
          </div>
          <div className="topbar-controls">
            <span className={"mode-label " + (mode === "demo" ? "demo" : "")}>
              <i />
              {mode === "demo" ? "Demo data" : "Live data"}
            </span>
            <label className="zone-label">
              <span className="sr-only">Display timezone</span>
              <select
                aria-label="Display timezone"
                value={zone}
                onChange={(e) => setZone(e.target.value as "UTC" | "local")}
              >
                <option>UTC</option>
                <option value="local">Local time</option>
              </select>
            </label>
            <button
              className="icon-button"
              onClick={toggleTheme}
              aria-label={light ? "Use dark theme" : "Use light theme"}
            >
              {light ? <Moon size={17} /> : <Sun size={17} />}
            </button>
          </div>
        </header>
        <main id="main">
          {mode === "demo" && (
            <div className="demo-banner">
              <FlaskConical size={15} />
              <strong>Demo data</strong>
              <span>
                Deterministic fixtures · September 23, 2026 · no gateway traffic
              </span>
              <button onClick={toggleMode}>Exit demo</button>
            </div>
          )}
          <div className="page-heading">
            <div>
              <div className="eyebrow">
                {detail
                  ? "REQUEST INTELLIGENCE"
                  : "GATEWAY / " + current.title.toUpperCase()}
              </div>
              <h1>{detail ? "Request investigation" : current.title}</h1>
              <p>
                {detail
                  ? "Inspect the evidence behind one inference."
                  : current.description}
              </p>
            </div>
            <div className="actions">
              {detail ? (
                <Link className="button" href={returnHref}>
                  <ArrowLeft size={14} />
                  {returnParams.get("view") === "chat"
                    ? "Back to chat"
                    : "Request explorer"}
                </Link>
              ) : current.key === "chat" ? null : (
                <>
                  <button
                    onClick={() => setPaused(!paused)}
                    aria-pressed={paused}
                  >
                    {paused ? <Play size={14} /> : <Pause size={14} />}
                    <span>{paused ? "Resume" : "Pause"} polling</span>
                  </button>
                  <button
                    aria-label="Refresh all panels"
                    className="icon-button"
                    onClick={() =>
                      client.invalidateQueries({
                        predicate: (q) => q.queryKey[0] === mode,
                      })
                    }
                  >
                    <RefreshCw size={15} />
                  </button>
                </>
              )}
            </div>
          </div>
          {children}
          <footer className="page-footer">
            <span>
              <span className="footer-dot" />
              Observations first. Evidence always.
            </span>
            <span>
              Time display:{" "}
              {zone === "UTC"
                ? "UTC"
                : Intl.DateTimeFormat().resolvedOptions().timeZone}
            </span>
          </footer>
        </main>
      </div>
    </div>
  );
}
export function Console({ requestId }: { requestId?: string }) {
  const params = useSearchParams(),
    { mode } = useRuntime();
  const view = params.get("view") ?? "overview";
  return (
    <Shell detail={!!requestId}>
      <div className="view-stack" key={mode}>
        {requestId ? (
          <section className="panel full-detail">
            <DetailBody id={requestId} />
          </section>
        ) : view === "chat" ? (
          <Chat />
        ) : view === "requests" ? (
          <Explorer />
        ) : view === "comparison" ? (
          <ComparisonView />
        ) : view === "reliability" ? (
          <Reliability />
        ) : view === "inventory" ? (
          <Inventory />
        ) : (
          <Overview />
        )}
      </div>
      {!requestId && <RequestDrawer />}
    </Shell>
  );
}
