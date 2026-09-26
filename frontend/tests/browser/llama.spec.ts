import { test, expect, type Page } from "@playwright/test";
import { demoModels, demoScan, demoStart, demoStatus, demoStop, demoRestart, demoLogs, demoSave, demoDelete, setDemoScenario } from "../../src/lib/llama-demo";

test("demo llama scenarios stay labelled and make no gateway calls", async ({ page }) => {
  let calls = 0;
  page.on("request", (request) => { if (request.url().includes("/api/llama-server/")) calls++; });
  await page.goto("/?view=llama&demo=1");
  await expect(page.getByRole("heading", { name: "llama-server status" })).toBeVisible();
  await expect(page.getByText("Demo data", { exact: true }).first()).toBeVisible();
  await page.getByLabel("Demo scenario").selectOption("disabled");
  await expect(page.getByRole("heading", { name: "llama-server management is not enabled" })).toBeVisible();
  await expect(page.getByText("scan_dirs", { exact: false })).toBeVisible();
  await page.getByLabel("Demo scenario").selectOption("ready");
  await expect(page.getByText("Model qwen-vl is ready")).toBeVisible();
  await expect(page.getByText("Context 32,768", { exact: false })).toBeVisible();
  await page.screenshot({ path: "verification/llama-ready.png", fullPage: true });
  await page.getByRole("button", { name: "Use light theme" }).click();
  await page.screenshot({ path: "verification/llama-ready-light.png", fullPage: true });
  await page.getByRole("button", { name: "Use dark theme" }).click();
  await page.getByLabel("Demo scenario").selectOption("external");
  await expect(page.getByText("External server", { exact: true })).toBeVisible();
  await expect(page.getByRole("button", { name: "Start", exact: true })).toBeDisabled();
  await page.getByLabel("Demo scenario").selectOption("exited");
  await expect(page.getByText("error: failed to load model")).toBeVisible();
  for (const state of ["stopped", "starting", "loading", "failed"]) {
    await page.getByLabel("Demo scenario").selectOption(state);
    await expect(page.getByText(`llama-server is ${state}`)).toBeVisible();
  }
  expect(calls).toBe(0);
});

async function mockLive(page: Page, calls: { path: string; method: string; body: unknown }[]) {
  setDemoScenario("stopped");
  await page.route("**/api/llama-server/**", async (route) => {
    const request = route.request();
    const url = new URL(request.url());
    const path = url.pathname.replace("/api/llama-server/", "");
    const method = request.method();
    const body = request.postDataJSON?.() ?? null;
    calls.push({ path, method, body });
    let data: unknown;
    let status = 200;
    if (path === "status") data = demoStatus();
    else if (path === "models" && method === "GET") data = demoModels();
    else if (path === "models" && method === "POST") { data = demoSave(body as Parameters<typeof demoSave>[0]); status = 201; }
    else if (path.startsWith("models/") && method === "PUT") data = demoSave(body as Parameters<typeof demoSave>[0], path.slice(7));
    else if (path.startsWith("models/") && method === "DELETE") { demoDelete(path.slice(7)); data = null; status = 204; }
    else if (path === "models/scan") data = demoScan();
    else if (path === "defaults") data = { args: demoModels().default_args };
    else if (path === "args/parse") data = { argv: String((body as { args?: string })?.args ?? "").split(/\s+/).filter(Boolean), issues: [] };
    else if (path === "models/validate") { const input = body as { alias: string; gguf_path: string }; data = input.gguf_path.toLowerCase().endsWith(".gguf") ? { valid: true, gguf_path: input.gguf_path, mmproj_path: null, alias_taken: false } : { valid: false, error: { code: "not_gguf", message: "Model file must end in .gguf" } }; }
    else if (path === "logs") data = demoLogs(url.searchParams.has("after") ? Number(url.searchParams.get("after")) : undefined);
    else if (path === "start") { const b = body as { model_id: string; args: string }; data = demoStart(b.model_id, b.args); status = 202; }
    else if (path === "stop") data = demoStop();
    else if (path === "restart") { data = demoRestart(); status = 202; }
    else data = { error: { code: "not_found", message: "Not found" } };
    await route.fulfill({ status, contentType: "application/json", body: JSON.stringify(data) });
  });
}

test("live launcher sends exact start, switch and stop requests", async ({ page }) => {
  const calls: { path: string; method: string; body: unknown }[] = [];
  await mockLive(page, calls);
  await page.goto("/?view=llama");
  await expect(page.getByRole("combobox", { name: "Saved model" })).toBeVisible();
  await page.getByRole("button", { name: "Start", exact: true }).first().click();
  await expect(page.getByText("llama-server is loading")).toBeVisible();
  const first = calls.find((call) => call.path === "start");
  expect(first?.body).toEqual({ model_id: demoModels().models[0].id, args: "-ngl all -fa on", replace: false, save_args: false });
  await page.getByRole("combobox", { name: "Saved model" }).selectOption(demoModels().models[1].id);
  await page.getByRole("textbox", { name: "Launch arguments" }).fill("-ngl 0 -c 512");
  await page.getByLabel("Save these arguments to this model").check();
  await page.getByRole("button", { name: "Switch to llama-8b" }).click();
  await expect(page.getByRole("dialog", { name: "Switch to llama-8b?" })).toBeVisible();
  await page.getByRole("button", { name: "Confirm" }).click();
  await expect.poll(() => calls.filter((call) => call.path === "start").length).toBe(2);
  expect(calls.filter((call) => call.path === "start")[1].body).toEqual({ model_id: demoModels().models[1].id, args: "-ngl 0 -c 512", replace: true, save_args: true });
  await page.getByRole("button", { name: "Stop", exact: true }).click();
  await expect.poll(() => calls.some((call) => call.path === "stop")).toBe(true);
  await expect(page.getByText("No model loaded")).toBeVisible();
  await page.getByRole("button", { name: "Restart", exact: true }).click();
  await expect(page.getByRole("dialog", { name: "Restart llama-server?" })).toBeVisible();
  await page.getByRole("button", { name: "Confirm" }).click();
  await expect.poll(() => calls.some((call) => call.path === "restart")).toBe(true);
});

test("model dialog validates paths and saves the full record", async ({ page }) => {
  const calls: { path: string; method: string; body: unknown }[] = [];
  await mockLive(page, calls);
  await page.goto("/?view=llama");
  await page.getByRole("button", { name: "Add model" }).click();
  const dialog = page.getByRole("dialog", { name: "Add a saved model" });
  await expect(dialog).toBeVisible();
  await dialog.getByRole("textbox", { name: "Model alias" }).fill("new-model");
  await dialog.getByRole("textbox", { name: "Model file path" }).fill("S:\\Code\\Models\\invalid.txt");
  await expect(dialog.getByText("Model file must end in .gguf", { exact: false })).toBeVisible();
  await expect(dialog.getByRole("button", { name: "Save model" })).toBeDisabled();
  await dialog.getByRole("button", { name: "Choose model from scanned files…" }).click();
  await dialog.getByRole("button", { name: /magicprompt.gguf/ }).click();
  await expect(dialog.getByRole("textbox", { name: "Model file path" })).toHaveValue("S:\\Code\\Models\\magicprompt.gguf");
  await expect(dialog.getByRole("button", { name: "Save model" })).toBeEnabled();
  await dialog.getByRole("button", { name: "Save model" }).click();
  await expect.poll(() => calls.some((call) => call.path === "models" && call.method === "POST")).toBe(true);
  expect(calls.find((call) => call.path === "models" && call.method === "POST")?.body).toEqual({ alias: "new-model", gguf_path: "S:\\Code\\Models\\magicprompt.gguf", mmproj_path: null, args: null, pass_alias: true, notes: null });
});

test("log cursor appends once and shows dropped output", async ({ page }) => {
  const calls: { path: string; method: string; body: unknown }[] = [];
  await mockLive(page, calls);
  await page.route("**/api/llama-server/logs**", async (route) => {
    const after = new URL(route.request().url()).searchParams.get("after");
    const lines = after === null ? [{ seq: 1, ts: "2026-09-23T08:40:00Z", stream: "stdout", text: "first log" }] : [{ seq: 2, ts: "2026-09-23T08:40:01Z", stream: "stderr", text: "second log" }];
    await route.fulfill({ contentType: "application/json", body: JSON.stringify({ lines, next_cursor: after === null ? 1 : 2, truncated: after !== null, state: "stopped" }) });
  });
  await page.goto("/?view=llama");
  const log = page.getByRole("log", { name: "llama-server logs" });
  await log.scrollIntoViewIfNeeded();
  await expect(log.getByText("first log", { exact: false })).toBeVisible();
  await page.getByRole("button", { name: "Refresh", exact: true }).last().click();
  await expect(log.getByText("second log", { exact: false })).toBeVisible();
  await expect(log.getByText("…older output was dropped")).toBeVisible();
  await page.getByRole("button", { name: "Refresh", exact: true }).last().click();
  await expect(log.getByText("second log", { exact: false })).toHaveCount(1);
});

test("a real same-origin browser fetch reaches the llama proxy guard", async ({ page }) => {
  await page.goto("/?view=llama&demo=1");
  const status = await page.evaluate(async () => (await fetch("/api/llama-server/status", { cache: "no-store" })).status);
  expect(status).not.toBe(403);
});
