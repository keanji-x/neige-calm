// Full production router, shell, file renderer and Conversation. Browser-only
// oracles below cover geometry, paint order, actual pointer hits and scrolling.
import '../../styles/entry.css';
import { act, cleanup, within } from '@testing-library/react';
import { commands, page, userEvent } from 'vitest/browser';
import { afterEach, expect, it } from 'vitest';
import { README_FILE_URL, README_TRACK_ID, README_TRACK_URL, renderReadmeFixture } from './report-readme-fixture.tsx';

declare module 'vitest/browser' {
  interface BrowserCommands {
    wheelScroll(selector: string, deltaY: number, position?: { x: number; y: number }): Promise<void>;
  }
}

afterEach(() => { cleanup(); document.getElementById('root')?.remove(); });

function fileLayer() { return document.querySelector<HTMLElement>('[data-nc-report-file-viewer]')!; }
function trackPage() { return document.querySelector<HTMLElement>('[data-nc-track-page]')!; }

function expectHit(element: Element) {
  const rect = element.getBoundingClientRect();
  expect(rect.width).toBeGreaterThan(0);
  expect(rect.height).toBeGreaterThan(0);
  const hit = document.elementFromPoint(rect.left + rect.width / 2, rect.top + rect.height / 2);
  expect(hit !== null && (hit === element || element.contains(hit)), `Covered: ${element.textContent}`).toBe(true);
}

async function visibleReadme() {
  const file = page.getByRole('region', { name: 'File docs/README.md' });
  await expect.element(file.getByRole('heading', { name: 'Documentation', exact: true })).toBeVisible();
  const layer = fileLayer();
  expect(within(layer).getAllByRole('table')).toHaveLength(2);
  expect(within(layer).getByRole('button', { name: 'Using Neige Calm' }).getAttribute('title')).toBe('docs/using-neige-calm.md');
  expect(layer.querySelector('pre')).toBeNull();
  expect(layer.querySelector('[data-nc-report-file-source]')).toBeNull();
  expect(layer.closest('[data-nc-track-page]')).toBe(trackPage());
  expect(layer.closest('[data-nc-conversation-drawer-host], [inert]')).toBeNull();
  const heading = within(layer).getByRole('heading', { name: 'Documentation' });
  expectHit(heading);
  const rect = layer.getBoundingClientRect();
  const parent = trackPage().getBoundingClientRect();
  expect(rect.height).toBeGreaterThan(200);
  expect(rect.top).toBeGreaterThanOrEqual(parent.top);
  expect(rect.bottom).toBeLessThanOrEqual(parent.bottom + 1);
  expect(rect.left).toBeGreaterThanOrEqual(parent.left);
  expect(rect.right).toBeLessThanOrEqual(parent.right + 1);
  expect(heading.getBoundingClientRect().width).toBeGreaterThan(180);
  expect(layer.scrollWidth).toBeLessThanOrEqual(layer.clientWidth + 1);
  return file;
}

// Hover the already visible scrollport's leading gutter, away from links and
// Conversation. No element reveal/scrollTop write sets up an interaction: the
// previous scrollIntoView({ block: 'center' }) also scrolled hidden ancestors.
async function wheel(surface: 'file' | 'report', deltaY: number) {
  const scroller = surface === 'file' ? fileLayer() : trackPage();
  const rect = scroller.getBoundingClientRect();
  const position = { x: 12, y: Math.floor(rect.height / 2) };
  const hit = document.elementFromPoint(rect.left + position.x, rect.top + position.y);
  expect(hit !== null && scroller.contains(hit), 'wheel target must be exposed').toBe(true);
  await commands.wheelScroll(surface === 'file' ? '[data-nc-report-file-viewer]' : '[data-nc-track-page]', deltaY, position);
}

async function scrollSettled(scroller: HTMLElement) {
  let previous = -1;
  let stable = 0;
  await expect.poll(() => {
    const current = scroller.scrollTop;
    stable = current === previous ? stable + 1 : 0;
    previous = current;
    return stable;
  }, { interval: 50 }).toBeGreaterThanOrEqual(3);
}

async function wheelToEdge(surface: 'file' | 'report', edge: 'start' | 'end') {
  const scroller = surface === 'file' ? fileLayer() : trackPage();
  await wheel(surface, (edge === 'start' ? -1 : 1) * scroller.scrollHeight);
  await expect.poll(() => Math.abs(scroller.scrollTop - (edge === 'start' ? 0 : scroller.scrollHeight - scroller.clientHeight)))
    .toBeLessThanOrEqual(1);
  await scrollSettled(scroller);
}

function expectReportPosition(reportScroll: number, action: string) {
  // Keep going on drift so the evidence also shows whether the document remains
  // reachable and whether close restores the Report. A failed value stays red.
  expect.soft(trackPage().scrollTop, `Report position after ${action}`).toBe(reportScroll);
}

async function readWithPointerAndKeyboard(reportScroll: number, screenshot: string) {
  const layer = fileLayer();
  await expect.poll(() => document.activeElement).toBe(layer);
  await wheel('file', 240);
  await expect.poll(() => layer.scrollTop).toBeGreaterThan(100);
  await scrollSettled(layer);
  expectReportPosition(reportScroll, 'wheel');
  const beforePageDown = layer.scrollTop;
  await userEvent.keyboard('{PageDown}');
  await expect.poll(() => layer.scrollTop).toBeGreaterThan(beforePageDown);
  await scrollSettled(layer);
  expectReportPosition(reportScroll, 'PageDown');
  await wheelToEdge('file', 'end');
  expectHit(within(layer).getByRole('button', { name: 'Prose ratchet' }));
  // Another wheel at the boundary must not move the Report or the app viewport.
  await wheel('file', 240);
  await scrollSettled(layer);
  expectReportPosition(reportScroll, 'wheel at document end');
  expect(document.scrollingElement?.scrollTop).toBe(0);
  await page.screenshot({ path: `./__screenshots__/report-readme-${screenshot}-wheel.png` });

  // From the file container, actual Tab must reveal each document control. This
  // exercises native focus scrolling, including the offscreen last table row.
  // The current document buttons are the oracle, not a copied Markdown fixture.
  for (const button of within(layer).getAllByRole('button')) {
    await userEvent.tab();
    expect(document.activeElement).toBe(button);
    expectHit(button);
  }
  await scrollSettled(layer);
  expectReportPosition(reportScroll, 'Tab through document links');
  await page.screenshot({ path: `./__screenshots__/report-readme-${screenshot}-tab.png` });

  // The document article is itself focusable. Clicking its exposed gutter
  // keeps focus inside the file before returning to the navigation link.
  await userEvent.click(layer, { position: { x: 12, y: 12 } });
  await expect.poll(() => layer.contains(document.activeElement)).toBe(true);
  await wheelToEdge('file', 'start');
  const parent = within(layer).getByRole('button', { name: 'English README' });
  expectHit(parent);
  await userEvent.click(parent);
  await expect.element(page.getByRole('region', { name: 'File README.md' })
    .getByRole('heading', { name: 'Why Neige Calm?', exact: true })).toBeVisible();
  expectReportPosition(reportScroll, 'document link click');
}

it.each([390, 1000, 1440])('cold-loads docs/README.md in the document viewport at %ipx', async width => {
  await page.viewport(width, 900);
  const { router, requests } = renderReadmeFixture(README_FILE_URL);
  await visibleReadme();
  expect(router.history.location.href).toBe(README_FILE_URL);
  expect(requests.filter(request => request.path.includes('/readfile')).map(request => request.path))
    .toEqual([`/api/tracks/${README_TRACK_ID}/workspace/readfile?path=docs%2FREADME.md`]);
  expect(document.documentElement.scrollWidth).toBeLessThanOrEqual(width);
  expect(trackPage().scrollTop).toBe(0);
  await page.screenshot({ path: `./__screenshots__/report-readme-cold-${width}.png` });
});

it('reads a cold file with wheel, PageDown, Tab and document links, then closes in place', async () => {
  await page.viewport(1440, 900);
  const { router } = renderReadmeFixture(README_FILE_URL);
  await visibleReadme();
  const reportScroll = trackPage().scrollTop;
  expect(reportScroll).toBe(0);
  await readWithPointerAndKeyboard(reportScroll, 'cold');
  expect(router.history.location.href).toBe(`${README_TRACK_URL}?file=README.md`);
  expect(router.history.length).toBe(1);
  await userEvent.keyboard('{Escape}');
  await expect.poll(() => router.history.location.href).toBe(README_TRACK_URL);
  await expect.element(page.getByRole('region', { name: 'File README.md' })).not.toBeInTheDocument();
  await scrollSettled(trackPage());
  expectReportPosition(reportScroll, 'cold close');
  expect(router.history.length).toBe(1);
});

it('reads from a wheel-scrolled Report with Conversation and restores its position and draft on close and Back', async () => {
  await page.viewport(1280, 900);
  const { router, requests } = renderReadmeFixture(README_TRACK_URL);
  await page.getByRole('button', { name: /Conversation Existing conversation/ }).click();
  const chat = page.getByRole('complementary', { name: 'Existing conversation' });
  await expect.element(chat.getByText('Conversation remains available while reading documentation.')).toBeVisible();
  await chat.getByRole('combobox', { name: 'Message' }).fill('Keep this draft');
  const conversation = chat.element();
  await Promise.all(conversation.getAnimations().map(animation => animation.finished));
  const chatBox = conversation.getBoundingClientRect();
  const chatScroller = conversation.querySelector<HTMLElement>('[data-nc-drawer-scroll]')!;
  const chatScroll = chatScroller.scrollTop;
  const opener = page.getByRole('button', { name: 'docs/README.md', exact: true });
  await wheelToEdge('report', 'end');
  expect(trackPage().scrollTop).toBeGreaterThan(100);
  expectHit(opener.element());
  const reportScroll = trackPage().scrollTop;
  await opener.click();
  await visibleReadme();
  expectReportPosition(reportScroll, 'open from scrolled Report');
  const reading = fileLayer().querySelector('[data-nc-report-reading]')!.getBoundingClientRect();
  expect(reading.right).toBeLessThanOrEqual(conversation.getBoundingClientRect().left);
  expectHit(chat.getByRole('combobox', { name: 'Message' }).element());
  await page.screenshot({ path: './__screenshots__/report-readme-conversation-1280.png' });

  await readWithPointerAndKeyboard(reportScroll, 'conversation');
  expect(router.history.location.href).toBe(`${README_TRACK_URL}?file=README.md`);
  expect(router.history.length).toBe(2);
  expect(chatScroller.scrollTop).toBe(chatScroll);
  expect(chat.element()).toBe(conversation);
  await expect.element(chat.getByRole('combobox', { name: 'Message' })).toHaveTextContent('Keep this draft');
  const back = page.getByRole('button', { name: 'Back to track', exact: true });
  expectHit(back.element());
  await back.click();
  await expect.poll(() => router.history.location.href).toBe(README_TRACK_URL);
  await expect.poll(() => document.activeElement).toBe(opener.element());
  await scrollSettled(trackPage());
  expectReportPosition(reportScroll, 'Back to track');
  expectHit(opener.element());
  expect(conversation.getBoundingClientRect().toJSON()).toEqual(chatBox.toJSON());
  await page.screenshot({ path: './__screenshots__/report-readme-conversation-restored.png' });

  await opener.click();
  await visibleReadme();
  act(() => { router.history.back(); });
  await expect.poll(() => router.history.location.href).toBe(README_TRACK_URL);
  await expect.element(page.getByRole('region', { name: 'File docs/README.md' })).not.toBeInTheDocument();
  await expect.poll(() => document.activeElement).toBe(opener.element());
  await scrollSettled(trackPage());
  expectReportPosition(reportScroll, 'history Back');
  expect(chat.element()).toBe(conversation);
  expect(chatScroller.scrollTop).toBe(chatScroll);
  await expect.element(chat.getByRole('combobox', { name: 'Message' })).toHaveTextContent('Keep this draft');
  expect(requests.every(request => request.method === 'GET')).toBe(true);
});

it('keeps the wide Conversation layout and foreground Escape when the document becomes compact', async () => {
  await page.viewport(1920, 900);
  const { router } = renderReadmeFixture(README_TRACK_URL);
  await page.getByRole('button', { name: /Conversation Existing conversation/ }).click();
  const chat = page.getByRole('complementary', { name: 'Existing conversation' });
  await chat.getByRole('combobox', { name: 'Message' }).fill('Compact draft');
  await page.getByRole('button', { name: 'docs/README.md', exact: true }).click();
  await visibleReadme();
  const layer = fileLayer();
  const reading = layer.querySelector('[data-nc-report-reading]')!.getBoundingClientRect();
  expect(reading.right).toBeLessThanOrEqual(chat.element().getBoundingClientRect().left);
  expectHit(chat.getByRole('combobox', { name: 'Message' }).element());
  await page.screenshot({ path: './__screenshots__/report-readme-conversation-1920.png' });
  await page.viewport(390, 844);
  await expect.element(chat.getByRole('combobox', { name: 'Message' })).toBeVisible();
  await expect.element(chat.getByRole('combobox', { name: 'Message' })).toHaveTextContent('Compact draft');
  expectHit(chat.getByRole('combobox', { name: 'Message' }).element());
  expect(document.documentElement.scrollWidth).toBeLessThanOrEqual(390);
  await page.screenshot({ path: './__screenshots__/report-readme-conversation-compact.png' });
  // Foreground Escape closes the Conversation first; the file route survives.
  await userEvent.keyboard('{Escape}');
  await expect.element(chat).not.toBeInTheDocument();
  expect(fileLayer()).toBe(layer);
  expect(router.history.location.href).toBe(README_FILE_URL);
  await visibleReadme();
  await userEvent.keyboard('{Escape}');
  await expect.poll(() => router.history.location.href).toBe(README_TRACK_URL);
});


for (const width of [390, 1000, 1440]) for (const conversation of [false, true]) for (const shortReport of [true, false]) {
  it(`recovers HTTP 500 at ${width}px, Conversation=${conversation}, short Report=${shortReport}`, async () => {
    // Open the real Conversation while wide before narrowing to mobile.
    await page.viewport(conversation ? 1440 : width, 900);
    const fixture = renderReadmeFixture(conversation ? README_TRACK_URL : README_FILE_URL, undefined,
      { shortReport, failFileRead: true });
    if (conversation) {
      await page.getByRole('button', { name: /Conversation Existing conversation/ }).click();
      const chat = page.getByRole('complementary', { name: 'Existing conversation' });
      await expect.element(chat.getByText('Conversation remains available while reading documentation.')).toBeVisible();
      await Promise.all(chat.element().getAnimations().map(animation => animation.finished));
      // The production link action preserves the mounted Conversation.
      await page.getByRole('button', { name: 'docs/README.md', exact: true }).click();
      await page.viewport(width, 900);
    }
    await expect.poll(() => fileLayer()?.querySelector('[role="alert"]') ?? null).not.toBeNull();
    if (conversation && width === 390) {
      // Mobile Conversation intentionally owns the foreground until dismissed.
      await userEvent.keyboard('{Escape}');
      await expect.element(page.getByRole('complementary', { name: 'Existing conversation' })).not.toBeInTheDocument();
    }
    const layer = fileLayer();
    const alert = within(layer).getByRole('alert');
    const name = layer.querySelector('[title="docs/README.md"]')!;
    const message = within(alert).getByText('Could not load this file.');
    const retry = within(alert).getByRole('button', { name: 'Retry' });
    const bounds = layer.getBoundingClientRect();
    const errorBounds = alert.getBoundingClientRect();
    const nameBounds = name.getBoundingClientRect();
    expect(errorBounds.left, 'error belongs to the filename document column').toBeCloseTo(nameBounds.left, 0);
    expect(errorBounds.top, 'error occupies the next row').toBeGreaterThanOrEqual(nameBounds.bottom - 1);
    expect(errorBounds.width, 'error must not collapse into the trailing gutter').toBeGreaterThan(180);
    expect(errorBounds.left).toBeGreaterThanOrEqual(bounds.left - 1);
    expect(errorBounds.right).toBeLessThanOrEqual(bounds.right + 1);
    expect(layer.scrollWidth).toBeLessThanOrEqual(layer.clientWidth + 1);
    // Hit tests precede click's automatic reveal; visibility alone misses covering chat.
    expectHit(message);
    expectHit(retry);
    if (conversation && width !== 390) {
      const chat = page.getByRole('complementary', { name: 'Existing conversation' });
      // Check the actual message and control against the covering edge.
      const chatLeft = chat.element().getBoundingClientRect().left;
      expect(message.getBoundingClientRect().right).toBeLessThanOrEqual(chatLeft);
      expect(retry.getBoundingClientRect().right).toBeLessThanOrEqual(chatLeft);
    }
    expect(alert.textContent).toBe('Could not load this file.Retry');
    await page.screenshot({ path: `./__screenshots__/report-readme-error-${width}-${conversation}-${shortReport}.png` });
    fixture.recoverFileRead();
    await userEvent.click(retry);
    await visibleReadme();
    expect(fixture.requests.filter(request => request.path.includes('/readfile')).map(request => request.path))
      .toEqual(Array<string>(2).fill(`/api/tracks/${README_TRACK_ID}/workspace/readfile?path=docs%2FREADME.md`));
    expect(fixture.router.history.location.href).toBe(README_FILE_URL);
  });
}
