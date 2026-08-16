#!/usr/bin/env node
/**
 * Manual drawer-head contrast audit; intentionally not run in CI.
 *
 * Requires a running preview, Playwright's Chromium, and a URL whose drawer is
 * already open with a representative real transcript long enough to scroll:
 *   node tools/styles/drawer-head-contrast-audit.mjs http://localhost:4173/waves/…
 *
 * For light and dark themes this samples 20 evenly spaced scroll positions.
 * At each position it hides the sticky head's title and buttons (leaving only
 * the composited frosted background), screenshots that background, and reports
 * the lowest pixel contrast against the title's computed text colour.
 */
import { chromium } from 'playwright';
import { createRequire } from 'node:module';

const { PNG } = createRequire(import.meta.url)('pngjs');

const url = process.argv[2];
if (!url) throw new Error('Pass the URL of a running preview with an open drawer.');

/** @param {number} value */
function channel(value) {
  const linear = value / 255;
  return linear <= 0.04045 ? linear / 12.92 : ((linear + 0.055) / 1.055) ** 2.4;
}

/** @param {[number, number, number]} colour */
function luminance([red, green, blue]) {
  return 0.2126 * channel(red) + 0.7152 * channel(green) + 0.0722 * channel(blue);
}

/** @param {[number, number, number]} first @param {[number, number, number]} second */
function contrast(first, second) {
  const [lighter, darker] = [luminance(first), luminance(second)].sort((a, b) => b - a);
  return (lighter + 0.05) / (darker + 0.05);
}

/** @param {string} cssColor @returns {[number, number, number]} */
function rgb(cssColor) {
  const values = cssColor.match(/[\d.]+/g)?.slice(0, 3).map(Number);
  if (!values || values.length !== 3) throw new Error(`Cannot parse colour: ${cssColor}`);
  return /** @type {[number, number, number]} */ (values);
}

const browser = await chromium.launch();
try {
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 }, deviceScaleFactor: 1 });
  await page.goto(url, { waitUntil: 'networkidle' });
  const drawer = page.getByRole('complementary');
  const title = drawer.getByRole('heading');
  await title.waitFor();
  const head = title.locator('..');
  const scroll = head.locator('..');

  for (const theme of ['light', 'dark']) {
    await page.evaluate((value) => { globalThis.document.documentElement.dataset.theme = value; }, theme);
    const foreground = rgb(await title.evaluate((element) => globalThis.getComputedStyle(element).color));
    const maximum = await scroll.evaluate((element) => element.scrollHeight - element.clientHeight);
    if (maximum <= 0) {
      throw new Error('Pass a URL with a transcript long enough for the drawer to actually scroll.');
    }
    let minimum = Number.POSITIVE_INFINITY;

    for (let index = 0; index < 20; index += 1) {
      await scroll.evaluate((element, top) => { element.scrollTop = top; }, maximum * index / 19);
      await page.evaluate(() => new Promise((resolve) => globalThis.requestAnimationFrame(() => resolve(undefined))));
      const hidden = await head.locator('h1, h2, h3, h4, h5, h6, button').evaluateAll((elements) => {
        for (const element of elements) element.style.visibility = 'hidden';
        return elements.length;
      });
      if (hidden === 0) throw new Error('Expected a drawer title or button to hide.');
      const image = PNG.sync.read(await head.screenshot());
      await head.locator('h1, h2, h3, h4, h5, h6, button')
        .evaluateAll((elements) => { for (const element of elements) element.style.visibility = ''; });
      for (let offset = 0; offset < image.data.length; offset += 4) {
        const background = /** @type {[number, number, number]} */ (
          [...image.data.subarray(offset, offset + 3)]
        );
        minimum = Math.min(minimum, contrast(foreground, background));
      }
    }
    console.log(`${theme}: ${minimum.toFixed(2)}:1 minimum across 20 positions`);
  }
} finally {
  await browser.close();
}
