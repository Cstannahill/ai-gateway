import { test, expect } from "@playwright/test";

test("Classic and Modern are independent of light and dark, and persist", async ({ page }) => {
  await page.goto("/?demo=1");
  const palette = () => page.locator("html").evaluate((element) => {
    const style = getComputedStyle(element);
    return {
      panel: style.getPropertyValue("--panel").trim(),
      accent: style.getPropertyValue("--accent").trim(),
      accentDim: style.getPropertyValue("--accent-dim").trim(),
      muted: style.getPropertyValue("--muted").trim(),
    };
  });
  expect((await palette()).accent).toBe("#51ccc5");
  await page.getByRole("button", { name: "Appearance: classic. Switch to Modern" }).click();
  await expect(page.locator("html")).toHaveAttribute("data-look", "modern");
  expect(await palette()).toMatchObject({ panel: "#5a5a5a", accent: "#cced00" });
  await page.screenshot({ path: "verification/modern-dark.png", fullPage: true });
  await page.getByRole("button", { name: "Use light theme" }).click();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "light");
  expect(await palette()).toMatchObject({ accentDim: "#cced00", muted: "#5a5a5a" });
  await page.screenshot({ path: "verification/modern-light.png", fullPage: true });
  await page.reload();
  await expect(page.locator("html")).toHaveAttribute("data-look", "modern");
  await expect(page.locator("html")).toHaveAttribute("data-theme", "light");
  await page.getByRole("button", { name: "Appearance: modern. Switch to Classic" }).click();
  await expect(page.locator("html")).toHaveAttribute("data-look", "classic");
  expect((await palette()).accent).toBe("#087b79");
});

test("appearance toggle fits the narrow header", async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto("/?demo=1");
  await page.getByRole("button", { name: "Appearance: classic. Switch to Modern" }).click();
  await expect(page.locator("html")).toHaveAttribute("data-look", "modern");
  expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(390);
});
