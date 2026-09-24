import { test, expect } from "@playwright/test";
import { demoResponse } from "../../src/lib/demo";

test("chat demo chooses models, streams, branches regeneration, preserves sessions, and exports", async ({
  page,
}) => {
  let traffic = 0;
  page.on("request", (req) => {
    if (req.url().includes("/api/")) traffic++;
  });
  await page.goto("/?view=chat&demo=1");
  await expect(
    page.getByRole("heading", { name: "What are we working on?" }),
  ).toBeVisible();
  await page
    .getByRole("textbox", { name: "Message", exact: true })
    .fill("Explain gateway routing");
  await expect(
    page.getByRole("button", { name: "Send message" }),
  ).toBeDisabled();
  await page.getByRole("button", { name: "Choose a model" }).click();
  await page
    .getByRole("textbox", { name: "Search chat models" })
    .fill("coding-fast");
  await page.getByRole("button", { name: /coding-fast.*context/ }).click();
  await page.evaluate(() => window.scrollTo(0, 0));
  await page.screenshot({
    path: "verification/chat-desktop.png",
    fullPage: true,
  });
  await page.getByRole("button", { name: "Send message" }).click();
  await expect(
    page.getByRole("button", { name: "Stop response" }),
  ).toBeVisible();
  await expect(
    page.getByText("demonstration response", { exact: false }),
  ).toBeVisible();
  await expect(
    page.getByRole("button", { name: "Regenerate in new chat" }),
  ).toBeVisible();
  await page.getByText("Reasoning", { exact: false }).first().click();
  await expect(
    page.getByText("Demo reasoning only.", { exact: false }),
  ).toBeVisible();
  await page.getByRole("link", { name: "Overview", exact: true }).click();
  await page.getByRole("link", { name: "Chat", exact: true }).click();
  await expect(
    page.getByRole("article", { name: "Your message" }),
  ).toContainText("Explain gateway routing");
  await page.getByRole("button", { name: "Regenerate in new chat" }).click();
  await expect(
    page.getByRole("button", { name: "Stop response" }),
  ).toBeVisible();
  await page.getByRole("button", { name: "Stop response" }).click();
  await expect(page.getByText("Stopped · partial")).toBeVisible();
  await expect(page.locator(".chat-conversations > div")).toHaveCount(2);
  const download = page.waitForEvent("download");
  await page.getByRole("button", { name: "Export conversation" }).click();
  expect((await download).suggestedFilename()).toMatch(/^chat-.+\.json$/);
  expect(traffic).toBe(0);
  await page.reload();
  await expect(
    page.getByRole("heading", { name: "What are we working on?" }),
  ).toBeVisible();
});

test("live chat sends settings and completed history, renders safe markdown, and links the gateway request", async ({
  page,
}) => {
  await page.route("**/api/gateway/**", (route) => {
    const u = new URL(route.request().url());
    return route.fulfill({
      json: demoResponse(u.pathname.replace("/api/gateway", "") + u.search),
    });
  });
  const payloads: Record<string, unknown>[] = [];
  const id = "0199768a-6e00-7000-8000-000000000001";
  await page.route("**/api/chat", (route) => {
    payloads.push(route.request().postDataJSON());
    const frame = (extra: object) =>
      `data: ${JSON.stringify({ id: "chatcmpl-" + id, model: "resolved-model", choices: [], ...extra })}\n\n`;
    return route.fulfill({
      contentType: "text/event-stream",
      body:
        frame({
          choices: [
            {
              index: 0,
              delta: {
                content:
                  "**Hello**\n\n```js\nconst answer = 42;\n```\n\n![tracking](https://invalid.test/pixel)\n<script>alert(1)</script>",
                reasoning_content: "Separate reasoning",
              },
              finish_reason: null,
            },
          ],
        }) +
        frame({
          usage: { prompt_tokens: 10, completion_tokens: 20, total_tokens: 30 },
        }) +
        frame({ choices: [{ index: 0, delta: {}, finish_reason: "stop" }] }) +
        "data: [DONE]\n\n",
    });
  });
  await page.goto("/?view=chat");
  await page.getByRole("button", { name: "Choose a model" }).click();
  await page
    .getByRole("textbox", { name: "Model ID or gateway alias" })
    .fill("coding-fast");
  await page.getByRole("button", { name: "Use model", exact: true }).click();
  await page
    .getByRole("button", { name: "Generation settings", exact: true })
    .click();
  await page.getByLabel("System prompt", { exact: true }).fill("Be precise.");
  await page.getByLabel("Max completion tokens").fill("50000");
  await page.getByRole("button", { name: "Close dialog" }).click();
  await page.getByRole("textbox", { name: "Message", exact: true }).fill("Hi");
  await page.getByRole("button", { name: "Send message" }).click();
  await expect(
    page.getByRole("button", { name: "Regenerate in new chat" }),
  ).toBeVisible();
  await expect(page.locator(".chat-markdown strong")).toHaveText("Hello");
  expect(
    await page.locator(".chat-markdown img, .chat-markdown script").count(),
  ).toBe(0);
  await expect(page.getByText("10 input · 20 output tokens*")).toBeVisible();
  expect(payloads[0]).toMatchObject({
    model: "coding-fast",
    max_completion_tokens: 50000,
    messages: [
      { role: "system", content: "Be precise." },
      { role: "user", content: "Hi" },
    ],
  });
  await page.getByRole("link", { name: "Inspect request" }).click();
  await expect(page.getByRole("dialog")).toBeVisible();
  await expect(page).toHaveURL(new RegExp("request=" + id));
  await page.keyboard.press("Escape");
  await expect(page).toHaveURL(/view=chat/);
  await page
    .getByRole("textbox", { name: "Message", exact: true })
    .fill("Follow up");
  await page.getByRole("button", { name: "Send message" }).click();
  await expect(
    page.getByRole("article", { name: "Assistant response" }),
  ).toHaveCount(2);
  expect((payloads[1].messages as unknown[]).length).toBe(4);
  await page.evaluate(() => window.scrollTo(0, 0));
  await page.screenshot({
    path: "verification/chat-response.png",
    fullPage: true,
  });
});

test("Next chat route enforces same-origin JSON and rejects unsupported payloads before inference", async ({
  request,
}) => {
  const invalid = {
    model: "test",
    messages: [{ role: "user", content: "test" }],
    stream: true,
    n: 2,
  };
  const crossSite = await request.post("/api/chat", {
    headers: { Origin: "https://other.test" },
    data: invalid,
  });
  expect(crossSite.status()).toBe(403);
  const sameSite = await request.post("/api/chat", {
    headers: { Origin: "http://127.0.0.1:3000" },
    data: invalid,
  });
  expect(sameSite.status()).toBe(400);
  expect((await sameSite.json()).error.message).toContain("single");
});

test("partial stream errors stay visible with no automatic retries; demo and live sessions stay separate", async ({
  page,
}) => {
  let attempts = 0;
  await page.route("**/api/gateway/**", (route) =>
    route.fulfill({ status: 503, json: { error: { message: "Offline" } } }),
  );
  await page.route("**/api/chat", (route) => {
    attempts++;
    return route.fulfill({
      contentType: "text/event-stream",
      body: 'data: {"choices":[{"index":0,"delta":{"content":"Partial answer"}}]}\n\ndata: {"error":{"message":"Provider timed out"}}\n\ndata: [DONE]\n\n',
    });
  });
  await page.goto("/?view=chat");
  await page.getByRole("button", { name: "Choose a model" }).click();
  await page
    .getByRole("textbox", { name: "Model ID or gateway alias" })
    .fill("local-alias");
  await page.getByRole("button", { name: "Use model", exact: true }).click();
  await page
    .getByRole("textbox", { name: "Message", exact: true })
    .fill("Live prompt");
  await page.getByRole("button", { name: "Send message" }).click();
  await expect(page.locator('.chat-error[role="alert"]')).toHaveText(
    "Provider timed out",
  );
  await expect(page.getByText("Partial answer", { exact: true })).toBeVisible();
  expect(attempts).toBe(1);
  await page.getByRole("button", { name: "Explore demo data" }).click();
  await expect(
    page.getByRole("heading", { name: "What are we working on?" }),
  ).toBeVisible();
  await expect(page.getByText("Live prompt", { exact: true })).toHaveCount(0);
  await page.getByRole("button", { name: "Return to live data" }).click();
  await expect(page.getByText("Partial answer", { exact: true })).toBeVisible();
});

test("mobile chat has accessible settings, history, and no horizontal overflow", async ({
  page,
}) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto("/?view=chat&demo=1");
  await page.getByRole("button", { name: "Use light theme" }).click();
  await page.getByRole("button", { name: "Toggle conversations" }).click();
  await expect(
    page.getByRole("button", { name: "New chat", exact: true }),
  ).toBeVisible();
  await page.getByRole("button", { name: "New chat", exact: true }).click();
  await expect(
    page.getByRole("button", { name: "Toggle conversations" }),
  ).toHaveAttribute("aria-expanded", "false");
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= innerWidth,
    ),
  ).toBe(true);
  await page.screenshot({
    path: "verification/chat-mobile-light.png",
    fullPage: true,
  });
});
