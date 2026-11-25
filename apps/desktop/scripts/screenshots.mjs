// Walks the mock UI (VITE_MOCK=1) through every screen in light and dark mode at the minimum
// window size and saves PNGs to docs/screenshots/ (git-ignored: generated locally, never committed).
//
//   corepack pnpm --filter desktop dev:mock      # in one terminal
//   corepack pnpm --filter desktop screenshots   # in another
//
// Uses the installed Google Chrome through playwright-core (no browser download).
// Env: APP_URL (default http://localhost:1420), CHROME_CHANNEL (default "chrome").

import { mkdir } from "node:fs/promises";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright-core";

const URL = process.env.APP_URL ?? "http://localhost:1420";
const OUT = join(dirname(fileURLToPath(import.meta.url)), "..", "docs", "screenshots");
// Matches minWidth/minHeight in src-tauri/tauri.conf.json.
const VIEWPORT = { width: 880, height: 600 };

async function shoot(page, scheme, name) {
  // Let transitions and tooltips settle.
  await page.mouse.move(0, VIEWPORT.height - 1);
  await page.waitForTimeout(250);
  const path = join(OUT, `${scheme}-${name}.png`);
  await page.screenshot({ path });
  console.log("saved", path);
}

async function run(browser, scheme) {
  const context = await browser.newContext({ viewport: VIEWPORT, deviceScaleFactor: 2, colorScheme: scheme });
  const page = await context.newPage();
  await page.goto(URL);
  await page.getByText("Startup disk").waitFor();
  await page.getByText("Free space").first().waitFor();

  // Overview → scan with live progress → scanned overview.
  await page.getByRole("button", { name: "Scan", exact: true }).first().click();
  await page.getByText(/of \d+ rules/).waitFor();
  await page.waitForTimeout(900);
  await shoot(page, scheme, "02-scan-progress");
  await page.getByRole("button", { name: "Scan Again" }).waitFor({ timeout: 15_000 });
  await shoot(page, scheme, "01-overview");

  // Findings: grouped list, blocked item, then a mixed selection.
  await page.getByRole("button", { name: /^Findings/ }).click();
  await shoot(page, scheme, "03-findings");
  for (const name of ["Select vm_bundles", "Select Docker reclaimable data", "Select set example-app"]) {
    await page.getByRole("checkbox", { name, exact: true }).check();
  }
  await page.getByRole("checkbox", { name: "Select mongo_28_01_2025", exact: true }).check();
  await page.getByText("Claude is running").first().scrollIntoViewIfNeeded();
  await page.getByText("AI tools", { exact: true }).scrollIntoViewIfNeeded();
  await shoot(page, scheme, "04-findings-selected");

  // Confirm: plan with warnings, Delete's second confirmation, then a Trash run with results.
  await page.getByRole("button", { name: "Review & Clean…" }).click();
  await page.getByText("Check before you continue").waitFor();
  await shoot(page, scheme, "05-confirm");
  await page.getByRole("radio", { name: /Delete permanently/ }).check();
  await page.getByRole("button", { name: "Delete…" }).click();
  await shoot(page, scheme, "06-confirm-delete");
  await page.getByRole("button", { name: "Back" }).click();
  await page.getByRole("radio", { name: /Move to Trash/ }).check();
  await page.getByRole("button", { name: "Move to Trash" }).click();
  await page.getByText("Clean finished").waitFor({ timeout: 15_000 });
  await shoot(page, scheme, "07-clean-result");
  await page.getByRole("button", { name: "Done" }).click();

  // History.
  await page.getByRole("button", { name: /^History/ }).click();
  await page.getByText("Growth by rule").waitFor();
  await page.waitForTimeout(300);
  await shoot(page, scheme, "08-history");

  // Overview after the clean, with Full Disk Access granted (mock) so the Trash size shows.
  await page.getByRole("button", { name: /^Overview/ }).click();
  await page.getByRole("button", { name: "Open Settings…" }).click();
  await page.getByRole("button", { name: "Check Again" }).click();
  await page.getByText("Still uses disk space").waitFor();
  await shoot(page, scheme, "09-overview-after-clean");

  // Storage: measured on first open, then one level down.
  await page.getByRole("button", { name: /^Storage/ }).click();
  await page.getByRole("button", { name: "Measure Again" }).waitFor({ timeout: 15_000 });
  await shoot(page, scheme, "10-storage");
  await page.getByRole("button", { name: /^~\/Library/ }).click();
  await page.getByText("Application Support").first().waitFor();
  await shoot(page, scheme, "11-storage-library");

  await context.close();
}

await mkdir(OUT, { recursive: true });
const browser = await chromium.launch({ channel: process.env.CHROME_CHANNEL ?? "chrome" });
try {
  for (const scheme of ["light", "dark"]) await run(browser, scheme);
} finally {
  await browser.close();
}
