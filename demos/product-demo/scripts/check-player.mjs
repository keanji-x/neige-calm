import assert from "node:assert/strict";
import { createRequire } from "node:module";
const { chromium } = createRequire(
  new URL("../../../fe/package.json", import.meta.url),
)("playwright");
const browser = await chromium.launch(),
  failures = [];
const url = process.argv[2] || "http://127.0.0.1:4188";
async function run(name, check) {
  const page = await browser.newPage({
    viewport: { width: 1600, height: 1000 },
  });
  const errors = [];
  page.on("pageerror", (e) => errors.push(e.message));
  try {
    await page.goto(url);
    await page.waitForFunction(() => window.demoDirector?.ready);
    const app = page.frames().find((f) => f.url().endsWith("/app.html"));
    const seek = (t) => page.evaluate((t) => window.demoDirector.seek(t), t);
    await check(page, app, seek);
    assert.deepEqual(errors, []);
    console.log("PASS " + name);
  } catch (error) {
    failures.push({ name, error: String(error) });
    console.error("FAIL " + name + ": " + error.message);
  } finally {
    await page.close();
  }
}
try {
  await run(
    "rewind to the post-create intro restores the initial report",
    async (page, app, seek) => {
      await seek(11);
      const initial = await app.locator("[data-nc-report-reading]").innerText();
      await seek(49);
      await seek(11);
      assert.equal(await app.locator('iframe[title="小店页面"]').count(), 0);
      assert.equal(
        await app.locator("[data-nc-report-reading]").innerText(),
        initial,
      );
      assert.equal(await page.evaluate(() => window.demoDirector.time), 11);
    },
  );
  await run(
    "concurrent late seek and reset leave no created sample",
    async (page, app, seek) => {
      await seek(0);
      await page.evaluate(() =>
        Promise.all([
          window.demoDirector.seek(49),
          window.demoDirector.seek(0),
        ]),
      );
      assert.equal(await page.evaluate(() => window.demoDirector.time), 0);
      assert.equal(
        await app.getByRole("button", { name: /New track in/ }).count(),
        0,
      );
      assert.equal(await app.locator("[data-nc-report]").count(), 0);
    },
  );
  await run(
    "rapid keyboard scrub uses the last selected time",
    async (page, app) => {
      const range = page.getByRole("slider", { name: "演示时间" });
      await range.focus();
      await range.press("End");
      await range.press("Home");
      if (
        await page.evaluate(
          () => typeof window.demoDirector.settled === "function",
        )
      )
        await page.evaluate(() => window.demoDirector.settled());
      else await page.waitForTimeout(1000);
      assert.equal(await range.inputValue(), "0");
      assert.equal(await page.evaluate(() => window.demoDirector.time), 0);
      assert.equal(
        await app.getByRole("button", { name: /New track in/ }).count(),
        0,
      );
    },
  );
  await run(
    "reset during a pending playback operation wins",
    async (page, app, seek) => {
      await seek(8.8);
      await app.evaluate(() => {
        const original = window.__neigeDemo.seek;
        window.__playbackEntered = false;
        let held = false;
        window.__neigeDemo.seek = async (target) => {
          if (target >= 9 && !held) {
            held = true;
            window.__playbackEntered = true;
            await new Promise((resolve) => {
              window.__releasePlayback = resolve;
            });
          }
          return original(target);
        };
      });
      await page.evaluate(() => window.demoDirector.play());
      await app.waitForFunction(() => window.__playbackEntered);
      await page.evaluate(() => {
        window.__resetPlayback = window.demoDirector.seek(0);
      });
      await app.evaluate(() => window.__releasePlayback());
      await page.evaluate(() => window.__resetPlayback);
      assert.equal(await page.evaluate(() => window.demoDirector.time), 0);
      assert.equal(
        await app.getByRole("button", { name: /New track in/ }).count(),
        0,
      );
    },
  );
  if (failures.length) throw Error(JSON.stringify(failures));
} finally {
  await browser.close();
}
