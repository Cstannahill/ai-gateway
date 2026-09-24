import { test, expect, type Page } from "@playwright/test";
import { demoResponse, demoRequests, demoMetrics } from "../../src/lib/demo";
async function fixtureLive(page: Page) {
  await page.route("**/api/gateway/**", async (route) => {
    const u = new URL(route.request().url());
    const path = u.pathname.replace("/api/gateway", "") + u.search;
    const data = demoResponse(path);
    await route.fulfill({
      status: data ? 200 : 404,
      contentType: "application/json",
      body: JSON.stringify(
        data ?? { error: { code: "not_found", message: "Request not found" } },
      ),
    });
  });
}
test("demo is explicit; overview has comparison, requests and scope labels", async ({
  page,
}) => {
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(e.message));
  let requests = 0;
  page.on("request", (r) => {
    if (r.url().includes("/api/gateway")) requests++;
  });
  await page.goto("/?demo=1");
  await expect(
    page.getByRole("heading", { name: "Provider performance" }),
  ).toBeVisible();
  await expect(
    page.getByRole("table", { name: "Recorded requests" }),
  ).toBeVisible();
  await expect(
    page.getByText("Since gateway restart", { exact: true }),
  ).toBeVisible();
  await expect(
    page.getByText("Demo data", { exact: true }).first(),
  ).toBeVisible();
  expect(requests).toBe(0);
  expect(errors).toEqual([]);
  await page.screenshot({
    path: "verification/overview-desktop.png",
    fullPage: true,
  });
});
test("filters, offset paging, detail drawer, safety and URL restoration", async ({
  page,
}) => {
  await page.goto("/?demo=1&view=requests");
  await expect(
    page.getByRole("table", { name: "Recorded requests" }),
  ).toBeVisible();
  await page.getByRole("button", { name: "Next page" }).click();
  await expect(page).toHaveURL(/offset=25/);
  await expect(page.getByText("25 loaded · offset 25")).toBeVisible();
  await page.getByRole("button", { name: "Next page" }).click();
  await expect(page.getByRole("button", { name: "Next page" })).toBeDisabled();
  await page.getByRole("button", { name: "Previous page" }).click();
  await page
    .getByRole("link", {
      name: demoRequests[25].request_id.slice(-12),
      exact: true,
    })
    .click();
  const dialog = page.getByRole("dialog");
  await expect(dialog).toBeVisible();
  await expect(
    dialog.getByText("Attempt chain", { exact: true }),
  ).toBeVisible();
  await dialog.getByRole("tab", { name: "Content", exact: true }).click();
  await expect(
    dialog.getByText('<script>alert("untrusted content")</script>', {
      exact: false,
    }),
  ).toBeVisible();
  expect(await dialog.locator("img,iframe,script").count()).toBe(0);
  await dialog.getByRole("tab", { name: "Timeline", exact: true }).click();
  await expect(
    dialog.getByRole("table", { name: "Exact timeline events" }),
  ).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(dialog).not.toBeVisible();
  await expect(page).toHaveURL(/offset=25/);
  await expect(page).not.toHaveURL(/request=/);
  await page
    .getByRole("textbox", { name: "Serving provider", exact: true })
    .fill("gemini");
  await page.getByRole("button", { name: "Apply filters" }).click();
  await expect(page).toHaveURL(/provider=gemini/);
  await expect(page).toHaveURL(/offset=0/);
  await page.reload();
  await expect(
    page.getByRole("textbox", { name: "Serving provider", exact: true }),
  ).toHaveValue("gemini");
});
test("request permalink, same-deployment retry, missing usage and synthetic timing", async ({
  page,
}) => {
  await page.goto(`/requests/${demoRequests[0].request_id}?demo=1`);
  await expect(
    page.getByText("Same-deployment retry", { exact: true }),
  ).toBeVisible();
  await page.getByRole("tab", { name: "Timeline", exact: true }).click();
  await page.screenshot({
    path: "verification/request-timeline.png",
    fullPage: true,
  });
  await page.goto(`/requests/${demoRequests[2].request_id}?demo=1`);
  await expect(
    page.getByText("Synthetic reconstruction", { exact: true }),
  ).toBeVisible();
  await page.goto(`/requests/${demoRequests[3].request_id}?demo=1`);
  await page.getByRole("tab", { name: "Usage", exact: true }).click();
  await expect(
    page.getByText("Usage not available", { exact: true }),
  ).toBeVisible();
});
test("full-page request link keeps explorer filters and pagination", async ({
  page,
}) => {
  await page.goto("/?demo=1&view=requests&offset=25&limit=25");
  await page
    .getByRole("link", {
      name: demoRequests[25].request_id.slice(-12),
      exact: true,
    })
    .click();
  await page.getByRole("link", { name: "Open full request page" }).click();
  await expect(page).toHaveURL(/\/requests\//);
  await page
    .getByRole("link", { name: "Request explorer", exact: true })
    .click();
  await expect(page).toHaveURL(/offset=25/);
  await expect(page.getByText("25 loaded · offset 25")).toBeVisible();
});
test("503 analytics remains unavailable while live operational data works", async ({
  page,
}) => {
  await fixtureLive(page);
  await page.route("**/api/gateway/v1/analytics/**", (route) =>
    route.fulfill({
      status: 503,
      contentType: "application/json",
      body: JSON.stringify({
        error: {
          code: "analytics_unavailable",
          message: "Database unavailable",
        },
      }),
    }),
  );
  await page.goto("/");
  await expect(
    page.getByText("Gateway requests", { exact: true }),
  ).toBeVisible();
  await expect(
    page.getByText("Analytics unavailable", { exact: true }).first(),
  ).toBeVisible();
  await expect(page.getByText("Demo data", { exact: true })).toHaveCount(0);
  await expect(
    page.getByText("No matching records", { exact: true }),
  ).toHaveCount(0);
});
test("404, empty next page, partial detail and offline states are distinct", async ({
  page,
}) => {
  await fixtureLive(page);
  await page.goto("/?view=requests&offset=100");
  await expect(
    page.getByText("No matching records", { exact: true }),
  ).toBeVisible();
  await expect(
    page.getByRole("button", { name: "Previous page" }),
  ).toBeEnabled();
  await page.goto("/requests/00000000-0000-0000-0000-000000000000");
  await expect(
    page.getByText("Request not found", { exact: true }),
  ).toBeVisible();
  await page.goto(`/requests/${demoRequests[6].request_id}`);
  await expect(
    page.getByText("Unfinished / unknown", { exact: true }),
  ).toBeVisible();
  await page.getByRole("tab", { name: "Content", exact: true }).click();
  await expect(
    page.getByText("No generation recorded yet", { exact: true }),
  ).toBeVisible();
  await page.unrouteAll();
  await page.route("**/api/gateway/**", (route) => route.abort());
  await page.goto("/");
  await expect(
    page.getByText("Gateway connection unavailable", { exact: true }).first(),
  ).toBeVisible();
  await expect(page.getByText("Demo data", { exact: true })).toHaveCount(0);
});
test("successful data remains visible on refresh failure", async ({ page }) => {
  await fixtureLive(page);
  await page.goto("/?view=requests");
  await expect(
    page.getByRole("table", { name: "Recorded requests" }),
  ).toBeVisible();
  await page.route("**/api/gateway/v1/analytics/requests?*", (route) =>
    route.fulfill({
      status: 503,
      contentType: "application/json",
      body: JSON.stringify({
        error: { code: "analytics_unavailable", message: "Unavailable" },
      }),
    }),
  );
  await page.getByRole("button", { name: "Refresh", exact: true }).click();
  await expect(page.getByText(/Stale data — refresh failed/)).toBeVisible({
    timeout: 12000,
  });
  await expect(
    page.getByRole("table", { name: "Recorded requests" }),
  ).toBeVisible();
});
test("head polling offers new rows without replacing the inspected page", async ({
  page,
}) => {
  await page.clock.install();
  await fixtureLive(page);
  let changed = false;
  await page.route("**/api/gateway/v1/analytics/requests?*", async (route) => {
    const data = {
      limit: 25,
      offset: 0,
      requests: changed
        ? [
            {
              ...demoRequests[0],
              request_id: "0199768a-6e00-7000-8000-ffffffffffff",
            },
            ...demoRequests.slice(0, 24),
          ]
        : demoRequests.slice(0, 25),
    };
    await route.fulfill({ json: data });
  });
  await page.goto("/?view=requests");
  await expect(
    page.getByRole("table", { name: "Recorded requests" }),
  ).toBeVisible();
  changed = true;
  await page.clock.fastForward(31000);
  await expect(
    page.getByRole("button", { name: "New requests available — load latest" }),
  ).toBeVisible();
  await expect(
    page.getByRole("link", { name: "ffffffffffff", exact: true }),
  ).toHaveCount(0);
  await page
    .getByRole("button", { name: "New requests available — load latest" })
    .click();
  await expect(
    page.getByRole("link", { name: "ffffffffffff", exact: true }),
  ).toBeVisible();
});
test("narrow layouts, navigation, light theme and dialog keyboard focus", async ({
  page,
}) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto("/?demo=1");
  await expect(
    page.getByRole("table", { name: "Recorded requests" }),
  ).toBeVisible();
  expect(
    await page.evaluate(() => document.documentElement.scrollWidth),
  ).toBeLessThanOrEqual(390);
  await page.getByRole("button", { name: "Use light theme" }).click();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "light");
  await page.screenshot({
    path: "verification/overview-mobile-light.png",
    fullPage: true,
  });
  await page.getByRole("button", { name: "Open navigation" }).click();
  await page.getByRole("link", { name: "Reliability", exact: true }).click();
  await expect(
    page.getByRole("heading", { name: "Recovery paths" }),
  ).toBeVisible();
  await page
    .getByRole("link", {
      name: demoRequests[0].request_id.slice(-12),
      exact: true,
    })
    .click();
  const dialog = page.getByRole("dialog");
  await expect(dialog).toBeVisible();
  await page.keyboard.press("Shift+Tab");
  expect(
    await dialog.evaluate((el) => el.contains(document.activeElement)),
  ).toBe(true);
  await page.keyboard.press("Escape");
  await expect(dialog).not.toBeVisible();
});
test("comparison selection, inventory search and exports work", async ({
  page,
}) => {
  await page.goto("/?demo=1&view=comparison");
  await expect(
    page.getByRole("table", { name: "Provider comparison" }),
  ).toBeVisible();
  await page.getByLabel("Comparison metric").selectOption("avg_ttft_ms");
  const download = page.waitForEvent("download");
  await page.getByRole("button", { name: "Export comparison JSON" }).click();
  expect((await download).suggestedFilename()).toBe("gateway-comparison.json");
  await page.goto("/?demo=1&view=inventory");
  await page
    .getByRole("textbox", { name: "Search model catalog" })
    .fill("gemini");
  await expect(
    page
      .getByRole("table", { name: "Model capability catalog" })
      .getByRole("row"),
  ).toHaveCount(2);
});
test("proxy rejects unsupported paths and parameters without reaching gateway", async ({
  request,
}) => {
  for (const path of [
    "/api/gateway/v1/chat/completions",
    "/api/gateway/health?url=http://example.com",
    "/api/gateway/v1/analytics/requests?from=2026-01-01",
  ])
    expect((await request.get(path)).status()).toBe(400);
  expect((await request.post("/api/gateway/health")).status()).toBe(405);
});
test("counter polling labels in-flight counts as provider attempts", async ({
  page,
}) => {
  await fixtureLive(page);
  await page.goto("/");
  await expect(
    page.getByText("Active provider attempts", { exact: true }),
  ).toBeVisible();
  await expect(
    page.getByText(
      String(demoMetrics.gateway.total_requests).replace(
        /\B(?=(\d{3})+(?!\d))/g,
        ",",
      ),
      { exact: true },
    ),
  ).toBeVisible();
});
