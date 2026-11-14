import assert from "node:assert/strict";
import fs from "node:fs";
import { createRequire } from "node:module";
const { chromium } = createRequire(
  new URL("../../../fe/package.json", import.meta.url),
)(process.argv[3] || "playwright");
const browser = await chromium.launch(),
  page = await browser.newPage({ viewport: { width: 1600, height: 1000 } }),
  errors = [],
  resources = [],
  checks = [];
page.on("pageerror", (e) => errors.push(e.message));
page.on("request", (r) => resources.push(r.url()));
try {
  await page.goto(process.argv[2] || "http://127.0.0.1:4188");
  await page.waitForFunction(() => window.demoDirector?.ready);
  const app = page.frames().find((f) => f.url().endsWith("/app.html"));
  const seek = async (t) => {
    await page.evaluate(
      (t) => window.demoDirector.seek(t),
      t > 21.5 ? t + 6 : t,
    );
    await page.waitForTimeout(70);
  };
  await seek(0);
  assert.equal(
    await app.getByRole("button", { name: /New track in/ }).count(),
    0,
  );
  await seek(1.2);
  assert.equal(
    (await app.locator("[role=dialog] input").inputValue()).length,
    1,
  );
  await seek(2.8);
  assert.equal(
    await app.locator("[role=dialog] input").inputValue(),
    "像素小店 · 演示项目",
  );
  await seek(3.5);
  assert.equal(
    await app
      .getByRole("button", {
        name: "New track in 像素小店 · 演示项目",
        exact: true,
      })
      .count(),
    1,
  );
  checks.push("native Area form creates one isolated sample Area");
  await seek(5.1);
  assert.equal(
    (
      await app
        .locator("[data-nc-new-track-message] [contenteditable]")
        .innerText()
    ).length,
    1,
  );
  await seek(8);
  assert.match(
    await app
      .locator("[data-nc-new-track-message] [contenteditable]")
      .innerText(),
    /再附一份使用说明/,
  );
  await seek(9.5);
  assert.equal(
    await app
      .getByRole("button", { name: "Track 小店页面 · 暗色模式", exact: true })
      .count(),
    1,
  );
  assert.match(await app.locator("body").innerText(), /再附一份使用说明/);
  checks.push(
    "native Track composer expands a character into the full request and submits it",
  );
  await seek(13.5);
  const before = await app.locator("[data-nc-report-reading]").innerText();
  await app.evaluate(() => {
    window.__reportIdentity = document.querySelector("[data-nc-report]");
    window.__samePage = "same";
  });
  await seek(15);
  assert.ok(
    (await app.locator("[data-nc-report-reading]").innerText()).length >
      before.length,
  );
  checks.push("Codex thoughts become native report content");
  await seek(17);
  assert.match(await app.locator("[data-nc-panel]").innerText(), /暗色模式/);
  checks.push(
    "new native Progress section is expanded so task checks remain visible",
  );
  await seek(19.5);
  assert.equal(
    await app.locator("[contenteditable][aria-label=Message]").innerText(),
    "两",
  );
  const panel = await app.locator("[data-nc-panel]").boundingBox(),
    drawer = await app.locator("[data-nc-drawer]").boundingBox();
  assert.ok(panel && drawer && panel.y < drawer.y);
  assert.equal(
    await app
      .locator("[data-nc-panel]")
      .evaluate((e) => getComputedStyle(e).visibility),
    "visible",
  );
  await seek(20.6);
  assert.equal(
    await app.locator("[contenteditable][aria-label=Message]").innerText(),
    "两种都要。",
  );
  await seek(21.1);
  assert.equal(
    await app.locator("[contenteditable][aria-label=Message]").innerText(),
    "",
  );
  await seek(21.15);
  assert.equal(
    await app.locator("[contenteditable][aria-label=Message]").innerText(),
    "",
  );
  await seek(21.5);
  assert.equal(
    await app
      .getByRole("button", { name: "Close conversation", exact: true })
      .count(),
    0,
  );
  checks.push(
    "decision is typed and submitted through the native conversation, with checks visible",
  );
  await page.evaluate(() => window.demoDirector.seek(22.5));
  assert.equal(
    await app.locator("[data-demo-relay]").getAttribute("data-owner"),
    "codex",
  );
  await page.evaluate(() => window.demoDirector.seek(24));
  assert.equal(
    await app.locator("[data-demo-relay]").getAttribute("data-owner"),
    "claude",
  );
  assert.equal(await app.locator("[data-demo-claude]").count(), 1);
  await page.evaluate(() => window.demoDirector.seek(26.4));
  assert.equal(
    await app.locator("[data-demo-relay]").getAttribute("data-owner"),
    "codex",
  );
  await page.evaluate(() => window.demoDirector.seek(28.04));
  assert.equal(
    await app.locator("[data-demo-relay]").getAttribute("data-owner"),
    "report",
  );
  assert.equal(await app.locator("[data-demo-relay-flash]").count(), 1);
  checks.push(
    "Codex delegates to Claude, catches the completed artifact and deposits it into Report",
  );
  await seek(23);
  assert.equal(await app.locator("[data-demo-thought]").count(), 1);
  assert.doesNotMatch(
    await app.locator("[data-nc-report-reading]").innerText(),
    /系统、浅色、深色三种外观/,
  );
  await seek(23.23);
  assert.equal(await app.locator("[data-demo-thought]").count(), 0);
  assert.match(
    await app.locator("[data-nc-report-reading]").innerText(),
    /系统、浅色、深色三种外观/,
  );
  checks.push("thought lands at the exact time its real content appears");
  await seek(28.5);
  assert.equal(await app.locator("[data-nc-report-file-viewer]").count(), 0);
  assert.match(
    await app.locator("[data-nc-link-preview]").innerText(),
    /像素小店使用说明/,
  );
  assert.equal(await app.locator("[data-demo-delivery]").count(), 1);
  checks.push("robot hover opens the native Markdown preview without clicking");
  await seek(31.5);
  assert.equal(await app.locator("[data-demo-preview-parcel]").count(), 1);
  assert.equal(await app.locator('iframe[title="小店页面"]').count(), 0);
  await seek(33.5);
  assert.equal(await app.locator("[data-demo-preview-parcel]").count(), 0);
  assert.equal(
    await app.locator('iframe[title="小店页面"]').getAttribute("sandbox"),
    "allow-scripts allow-forms allow-popups",
  );
  checks.push(
    "live HTML preview parcel lands as the native sandboxed app block",
  );
  await seek(37);
  const site = page
    .frames()
    .find((f) => f.url().includes("/sample/index.html"));
  assert.equal(await site.locator("html").getAttribute("data-theme"), "dark");
  await seek(41);
  assert.equal(await site.locator("html").getAttribute("data-theme"), "light");
  await site.getByRole("button", { name: "使用深色外观", exact: true }).click();
  assert.equal(await site.locator("html").getAttribute("data-theme"), "dark");
  checks.push("delivered site remains directly interactive");
  const writes = (await app.evaluate(() => window.__neigeDemo.requests)).filter(
    (r) => r.method !== "GET" && !r.path.startsWith("/api/read-receipts/"),
  );
  assert.deepEqual(
    writes.map((r) => r.path),
    [
      "/api/areas",
      "/api/tracks",
      "/api/cards/44444444444444448444444444444444/planner/input",
    ],
  );
  assert.equal(writes[1].body.area_id, "22222222222242228222222222222222");
  assert.equal(writes[2].body.text, "两种都要。");
  assert.ok(!resources.some((u) => new URL(u).pathname.startsWith("/api/")));
  checks.push(
    "three native mutations are handled only in memory, with zero backend HTTP requests",
  );
  await seek(23);
  assert.ok(
    await app.evaluate(
      () =>
        window.__reportIdentity === document.querySelector("[data-nc-report]"),
    ),
  );
  await seek(0);
  assert.equal(
    await app.getByRole("button", { name: /New track in/ }).count(),
    0,
  );
  await seek(8);
  assert.match(
    await app.locator("[data-nc-new-track-message]").innerText(),
    /再附一份使用说明/,
  );
  assert.equal(await app.evaluate(() => window.__samePage), "same");
  checks.push(
    "rewind reconstructs creation state without reloading or leaking a prior project",
  );
  assert.deepEqual(
    resources.filter((u) => /\.(jpg|png|webm|mp4)(\?|$)|robot-frames/.test(u)),
    [],
  );
  assert.deepEqual(errors, []);
  checks.push("no prerecorded source frames or browser errors");
  fs.writeFileSync(
    "output/story-integration.json",
    JSON.stringify(
      {
        passed: true,
        checks,
        backendRequests: 0,
        sourceMediaInputs: 0,
        errors,
      },
      null,
      2,
    ),
  );
  console.log(JSON.stringify({ passed: true, checks: checks.length }));
} finally {
  await browser.close();
}
