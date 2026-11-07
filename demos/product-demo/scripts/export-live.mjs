import fs from "node:fs";
import path from "node:path";
import { spawn } from "node:child_process";
import { createRequire } from "node:module";
import { once } from "node:events";
const require = createRequire(
    new URL("../../../fe/package.json", import.meta.url),
  ),
  { chromium } = require(process.argv[4] || "playwright");
const url = process.argv[2] || "http://127.0.0.1:4187",
  out = path.resolve(process.argv[3] || "output/neige-live-frontend.mp4");
const { duration, fps } = JSON.parse(
  fs.readFileSync(new URL("../live/spec.json", import.meta.url), "utf8"),
);
fs.mkdirSync(path.dirname(out), { recursive: true });
const browser = await chromium.launch({ headless: true });
const page = await browser.newPage({
  viewport: { width: 1600, height: 1000 },
  deviceScaleFactor: 1,
});
const resources = [],
  errors = [];
page.on("request", (r) => resources.push(r.url()));
page.on("pageerror", (e) => errors.push(e.message));
let encoder;
try {
  await page.goto(url);
  await page.waitForFunction(() => window.demoDirector?.ready, {
    timeout: 30000,
  });
  const native = page.frames().find((f) => f.url().endsWith("/app.html"));
  await page.evaluate(() => {
    window.demoDirector.pause();
    window.demoDirector.exportMode();
    return document.fonts.ready;
  });
  await native.evaluate(() => document.fonts.ready);
  const cdp = await page.context().newCDPSession(page);
  encoder = spawn(
    "ffmpeg",
    [
      "-hide_banner",
      "-loglevel",
      "error",
      "-y",
      "-f",
      "image2pipe",
      "-framerate",
      String(fps),
      "-i",
      "pipe:0",
      "-stream_loop",
      "-1",
      "-i",
      path.resolve("public/score.wav"),
      "-t",
      String(duration),
      "-filter:a",
      `volume=0.22,afade=t=in:d=0.5,afade=t=out:st=${duration - 2}:d=2`,
      "-c:v",
      "libx264",
      "-preset",
      "fast",
      "-crf",
      "19",
      "-pix_fmt",
      "yuv420p",
      "-threads",
      "4",
      "-c:a",
      "aac",
      "-b:a",
      "128k",
      "-movflags",
      "+faststart",
      out,
    ],
    { stdio: ["pipe", "inherit", "inherit"] },
  );
  const ended = once(encoder, "close");
  for (let f = 0; f < duration * fps; f++) {
    await page.evaluate((t) => window.demoDirector.seek(t), f / fps);
    const image = await cdp.send("Page.captureScreenshot", {
      format: "png",
      captureBeyondViewport: false,
    });
    if (!encoder.stdin.write(Buffer.from(image.data, "base64")))
      await once(encoder.stdin, "drain");
    if (f % 120 === 0) console.log("Rendered live DOM", f, "/", duration * fps);
  }
  encoder.stdin.end();
  const [code] = await ended;
  if (code !== 0) throw Error("Encoder failed");
  const raster = resources.filter((u) =>
    /\.(jpg|png|webm|mp4)(\?|$)|robot-frames/.test(u),
  );
  if (raster.length || errors.length)
    throw Error("Live export used unexpected media input or browser errors");
  fs.writeFileSync(
    path.join(path.dirname(out), "live-export.json"),
    JSON.stringify(
      {
        duration,
        fps,
        frames: duration * fps,
        renderer: "live production React DOM via controlled frontend clock",
        sourceScreenshots: 0,
        sourceVideos: 0,
        networkRasterOrVideoInputs: raster,
        browserErrors: errors,
      },
      null,
      2,
    ),
  );
  console.log("Live frontend export complete");
} finally {
  encoder?.stdin.end();
  await browser.close();
}
