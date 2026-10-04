import { page as browserPage, userEvent } from 'vitest/browser';
import { render, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import '../../../styles/entry.css';

import { useState } from '../../../ui/state/public.ts';
import { TrackPage } from './public.tsx';
import { renderPage, track } from './test-fixtures.tsx';

afterEach(() => {
  document.body.replaceChildren();
  delete document.documentElement.dataset.theme;
});

type Rgb = readonly [number, number, number];

function paintedRgb(cssColor: string): Rgb {
  const canvas = document.createElement('canvas');
  canvas.width = 1;
  canvas.height = 1;
  const context = canvas.getContext('2d', { willReadFrequently: true });
  if (context === null) throw new Error('no 2d canvas context');
  context.fillStyle = cssColor;
  context.fillRect(0, 0, 1, 1);
  const [r, g, b] = context.getImageData(0, 0, 1, 1).data;
  return [r, g, b];
}

function contrast(first: Rgb, second: Rgb): number {
  const channel = (byte: number) => {
    const value = byte / 255;
    return value <= 0.04045 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4;
  };
  const luminance = ([r, g, b]: Rgb) =>
    0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b);
  const [high, low] = [luminance(first), luminance(second)].sort((a, b) => b - a);
  return (high + 0.05) / (low + 0.05);
}

const plannerNotification = [{
  key: 'ask:ratify:1', kind: 'ask' as const, text: 'Merge PR #1811 now, or hold it?', atMs: 1,
}];

describe('the track closed status in the page header', () => {
  it('sits directly beside the title as quiet text', async () => {
    await browserPage.viewport(1200, 800);
    renderPage({ track: track({ title: 'Status preview', closedAt: 5 }) });

    const title = document.querySelector<HTMLElement>('[aria-label="Rename track"]')!;
    const status = document.querySelector<HTMLElement>('[aria-label="Track closed"]')!;
    const gap = status.getBoundingClientRect().left - title.getBoundingClientRect().right;

    expect(gap).toBeGreaterThanOrEqual(4);
    expect(gap).toBeLessThanOrEqual(12);
    expect(getComputedStyle(status).fontSize).toBe('11px');
    expect(title.scrollWidth).toBeLessThanOrEqual(title.clientWidth);
  });

  it('uses a transparent text-only treatment', async () => {
    await browserPage.viewport(1200, 800);
    renderPage({ track: track({ closedAt: 5 }), canReopenTrack: true });

    const status = document.querySelector<HTMLElement>('[aria-label="Track closed"]')!;
    const statusStyle = getComputedStyle(status);

    expect(status.getBoundingClientRect().height).toBe(24);
    expect(statusStyle.backgroundColor).toBe('rgba(0, 0, 0, 0)');
    expect(statusStyle.padding).toBe('0px');
    expect(statusStyle.borderTopWidth).toBe('0px');
  });

  it('keeps the three-dot action visible at the secondary text rank', async () => {
    await browserPage.viewport(1200, 800);
    renderPage();

    const actions = document.querySelector<HTMLElement>('[aria-label^="Track actions for "]')!;
    const text2 = getComputedStyle(document.documentElement).getPropertyValue('--text-2');
    expect(paintedRgb(getComputedStyle(actions).color)).toEqual(paintedRgb(text2));
  });

  it('floats a Planner ask at the viewport corner; the whole row answers it and its × dismisses it', async () => {
    await browserPage.viewport(1200, 800);
    const onReply = vi.fn();
    const onDismiss = vi.fn(() => Promise.resolve());
    renderPage({
      inputNotifications: [{
        key: 'ask:ratify:1', kind: 'ask', atMs: Date.now(),
        text: 'Merge PR #1811 now? See [the PR](https://example.com/pr/1811).',
      }],
      onReply,
      onDismiss,
    });

    const notice = document.querySelector<HTMLElement>('[data-nc-needs-input-notice]')!;
    const noticeBox = notice.getBoundingClientRect();
    expect(window.innerWidth - noticeBox.right).toBeGreaterThanOrEqual(20);
    expect(window.innerWidth - noticeBox.right).toBeLessThanOrEqual(28);
    expect(window.innerHeight - noticeBox.bottom).toBeGreaterThanOrEqual(20);
    expect(window.innerHeight - noticeBox.bottom).toBeLessThanOrEqual(28);

    const row = notice.querySelector<HTMLElement>('[data-nc-notification-state="ask"]')!;
    const open = row.querySelector<HTMLButtonElement>('[aria-label^="Answer the Planner: "]')!;
    const dismiss = row.querySelector<HTMLButtonElement>('[aria-label^="Dismiss: Needs your answer: "]')!;
    const link = row.querySelector<HTMLAnchorElement>('a')!;
    const time = row.querySelector<HTMLElement>('time')!;
    const label = [...row.querySelectorAll<HTMLElement>('span')].find((el) => el.textContent === 'Needs your answer')!;
    const action = [...row.querySelectorAll<HTMLElement>('span')].find((el) => el.textContent === 'Answer in Planner')!;
    /* The row button covers the whole row inside its divider; the body text is under it, the link and
       the × are above it. */
    const rowBox = row.getBoundingClientRect();
    const openBox = open.getBoundingClientRect();
    const inner = [rowBox.left + row.clientLeft, rowBox.top + row.clientTop, row.clientWidth, row.clientHeight];
    [openBox.left, openBox.top, openBox.width, openBox.height]
      .forEach((edge, i) => expect(Math.abs(edge - inner[i])).toBeLessThan(1));
    const centre = (el: Element) => {
      const box = el.getBoundingClientRect();
      return document.elementFromPoint(box.left + box.width / 2, box.top + box.height / 2);
    };
    const body = row.querySelector(':scope > div')!;
    expect(centre(body.firstElementChild!)).toBe(open);
    expect(centre(link)).toBe(link);
    expect(centre(dismiss)?.closest('button')).toBe(dismiss);
    expect(dismiss.title).toBe('Dismiss');
    /* The × is alone after the time; the label and the action share one slot at the start of the line. */
    expect(dismiss.getBoundingClientRect().left).toBeGreaterThanOrEqual(time.getBoundingClientRect().right);
    expect(action.getBoundingClientRect().left).toBe(label.getBoundingClientRect().left);
    /* The body is the row's last line: bottom padding equals top padding. */
    const rowStyle = getComputedStyle(row);
    expect(rowStyle.paddingBottom).toBe(rowStyle.paddingTop);

    /* At rest the label shows; hovering puts the action in its place. The time, the × corner and the
       row's height do not move. */
    const restHeight = row.getBoundingClientRect().height;
    const restTime = time.getBoundingClientRect().left;
    expect(getComputedStyle(label).visibility).toBe('visible');
    expect(getComputedStyle(action).visibility).toBe('hidden');
    expect(getComputedStyle(dismiss).opacity).toBe('0');
    await userEvent.hover(open);
    expect(getComputedStyle(label).visibility).toBe('hidden');
    expect(getComputedStyle(action).visibility).toBe('visible');
    const accent = getComputedStyle(document.documentElement).getPropertyValue('--accent');
    expect(paintedRgb(getComputedStyle(action).color)).toEqual(paintedRgb(accent));
    expect(getComputedStyle(time).visibility).toBe('visible');
    expect(time.getBoundingClientRect().left).toBe(restTime);
    expect(getComputedStyle(dismiss).opacity).toBe('1');
    expect(row.getBoundingClientRect().height).toBe(restHeight);
    expect(getComputedStyle(open).cursor).toBe('pointer');

    await userEvent.click(open);
    expect(onReply).toHaveBeenCalledOnce();
    await userEvent.click(dismiss);
    expect(onDismiss).toHaveBeenCalledWith('ask:ratify:1');
    expect(onReply).toHaveBeenCalledOnce();
  });

  it('clamps a planner-down reason to three lines and never an ask', async () => {
    await browserPage.viewport(1200, 800);
    const long = Array.from({ length: 12 }, (_, i) => `Line ${i} of a long upstream error body.`).join(' ');
    renderPage({
      inputNotifications: [
        { key: 'ask:notify:1', kind: 'ask', atMs: 2, text: long },
        { key: 'planner_down:2', kind: 'planner-down', atMs: 1, text: long },
      ],
      onReply: vi.fn(),
    });
    /* A row's body is its one direct `div` child. */
    const lines = (body: HTMLElement) => body.getBoundingClientRect().height / parseFloat(getComputedStyle(body).lineHeight);
    const down = document.querySelector<HTMLElement>('[data-nc-notification-state="planner-down"] > div')!;
    const ask = document.querySelector<HTMLElement>('[data-nc-notification-state="ask"] > div')!;
    expect(lines(down)).toBeGreaterThan(2.5);
    expect(lines(down)).toBeLessThanOrEqual(3.05);
    expect(lines(ask)).toBeGreaterThan(3.5);
  });

  it('keeps three type steps: the title largest, the body at body size, the meta line smallest', async () => {
    await browserPage.viewport(1200, 800);
    renderPage({
      inputNotifications: [{ key: 'ask:notify:1', kind: 'ask', atMs: 1, text: '# Heading\n\nRun `deploy.sh` now?' }],
      onReply: vi.fn(),
    });
    const notice = document.querySelector<HTMLElement>('[data-nc-needs-input-notice]')!;
    const row = notice.querySelector<HTMLElement>('[data-nc-notification-state]')!;
    const size = (el: Element) => getComputedStyle(el).fontSize;
    const tokens = getComputedStyle(document.documentElement);
    const token = (name: string) => tokens.getPropertyValue(name).trim();
    expect(size([...notice.querySelectorAll('strong')].find((el) => el.textContent === 'Waiting on you')!)).toBe(token('--text-md'));
    expect(size(row.querySelector('h3')!)).toBe(token('--text-base'));
    expect(getComputedStyle(row.querySelector('h3')!).fontWeight).toBe(token('--weight-semibold'));
    expect(size(row.querySelector('code')!)).toBe(token('--text-xs'));
    expect(size(row.querySelector('time')!)).toBe(token('--text-xs'));
    expect(size(row.querySelector(':scope > div')!.querySelector('h3')!.nextElementSibling!)).toBe(token('--text-base'));
  });

  it('compacts an input notification beside an open conversation drawer', async () => {
    await browserPage.viewport(1200, 800);
    renderPage({
      inputNotifications: plannerNotification,
      conversationOpen: true,
    });

    const notice = document.querySelector<HTMLElement>('[data-nc-needs-input-notice]')!;
    const launcher = document.querySelector<HTMLButtonElement>('[data-nc-notification-launcher]')!;
    const box = notice.getBoundingClientRect();
    expect(notice.dataset.ncNotificationMode).toBe('compact');
    expect(box.width).toBe(40);
    expect(window.innerWidth - box.right).toBeGreaterThan(250);
    expect(notice.querySelector('strong')).toBeNull();
    await userEvent.click(launcher);
    expect(notice.dataset.ncNotificationMode).toBe('expanded');
  });

  it('returns focus to the Track actions button when Escape closes its menu', async () => {
    await browserPage.viewport(1200, 800);
    renderPage({ track: track({ closedAt: 5 }), canReopenTrack: true });
    const actions = document.querySelector<HTMLButtonElement>('[aria-label^="Track actions for "]')!;

    actions.click();
    await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
    expect(document.querySelector<HTMLElement>('[role="menu"]')?.closest('[popover]')
      ?.matches(':popover-open')).toBe(true);

    await userEvent.keyboard('{Escape}');
    await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
    expect(document.activeElement).toBe(
      document.querySelector('[aria-label^="Track actions for "]'),
    );
  });

  /* The server offers one of Close and Reopen; the harness flips `closedAt` and both capabilities together. */
  function ClosedStateHarness({ initiallyClosed }: { initiallyClosed: boolean }) {
    const [closed, setClosed] = useState(initiallyClosed);
    return (
      <TrackPage
        mobilePanelObscured={false}
        track={track({ closedAt: closed ? 5 : null })}
        cards={[]}
        tasks={[]}
        openableCards={new Set()}
        canReopenTrack={closed}
        canCloseTrack={!closed}
        onRenameTrack={vi.fn()}
        onReopenTrack={() => { setClosed(false); }}
        onCloseTrack={() => { setClosed(true); }}
        onDeleteTrack={vi.fn()}
      />
    );
  }

  /** Opens Track actions from the keyboard and activates its first item, which is the Close or Reopen action. */
  async function activateFirstTrackAction(): Promise<HTMLButtonElement> {
    const actions = document.querySelector<HTMLButtonElement>('[aria-label^="Track actions for "]')!;
    actions.focus();
    await userEvent.keyboard('{Enter}');
    await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
    expect(document.activeElement?.getAttribute('role')).toBe('menuitem');

    await userEvent.keyboard('{Enter}');
    await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
    return document.querySelector<HTMLButtonElement>('[aria-label^="Track actions for "]')!;
  }

  it('keeps keyboard focus on Track actions after Reopen removes the action', async () => {
    await browserPage.viewport(1200, 800);
    render(<ClosedStateHarness initiallyClosed />);
    const currentActions = await activateFirstTrackAction();
    expect(document.activeElement).toBe(currentActions);
    expect(currentActions.matches(':focus-visible')).toBe(true);
    expect(document.querySelector('[aria-label="Track closed"]')).toBeNull();
  });

  it('keeps keyboard focus on Track actions after Close removes the action', async () => {
    await browserPage.viewport(1200, 800);
    render(<ClosedStateHarness initiallyClosed={false} />);
    expect(document.querySelector('[aria-label="Track closed"]')).toBeNull();
    const currentActions = await activateFirstTrackAction();
    expect(document.activeElement).toBe(currentActions);
    expect(currentActions.matches(':focus-visible')).toBe(true);
    expect(document.querySelector('[aria-label="Track closed"]')).not.toBeNull();
  });

  it('light-dismisses Track actions on an outside click', async () => {
    await browserPage.viewport(1200, 800);
    renderPage({ track: track({ closedAt: 5 }), canReopenTrack: true });
    const actions = document.querySelector<HTMLButtonElement>('[aria-label^="Track actions for "]')!;
    const popover = () => document.querySelector<HTMLElement>('[role="menu"]')?.closest<HTMLElement>('[popover]');

    actions.click();
    await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
    expect(popover()?.matches(':popover-open')).toBe(true);
    await userEvent.click(document.body);
    expect(popover()?.matches(':popover-open')).toBe(false);
  });

  it('truncates a long title while the input notice stays out of the header', async () => {
    /* Stay just above the 60rem mobile navigation cutover: below it the
       desktop PageHeader is intentionally replaced by MobileHeader. */
    await browserPage.viewport(1000, 600);
    renderPage({
      track: track({
        title: 'A deliberately long track title '.repeat(12),
        closedAt: 5,
      }),
      inputNotifications: plannerNotification,
    });

    const title = document.querySelector<HTMLElement>('[aria-label="Rename track"]')!;
    const status = document.querySelector<HTMLElement>('[aria-label="Track closed"]')!;
    const actions = document.querySelector<HTMLElement>('[aria-label^="Track actions for "]')!;
    const notice = document.querySelector<HTMLElement>('[data-nc-needs-input-notice]')!;
    const titleBox = title.getBoundingClientRect();
    const statusBox = status.getBoundingClientRect();

    expect(title.scrollWidth).toBeGreaterThan(title.clientWidth);
    expect(statusBox.left - titleBox.right).toBeGreaterThanOrEqual(4);
    expect(statusBox.left - titleBox.right).toBeLessThanOrEqual(12);
    expect(statusBox.right).toBeLessThan(actions.getBoundingClientRect().left);
    expect(notice.getBoundingClientRect().top).toBeGreaterThan(actions.getBoundingClientRect().bottom);
  });

  it('keeps the subdued closed colour readable in both themes', async () => {
    await browserPage.viewport(1200, 800);
    renderPage({ track: track({ closedAt: 5 }) });
    const status = document.querySelector<HTMLElement>('[aria-label="Track closed"]')!;

    for (const theme of ['light', 'dark'] as const) {
      document.documentElement.dataset.theme = theme;
      const foreground = paintedRgb(getComputedStyle(status).color);
      const background = paintedRgb(
        getComputedStyle(document.documentElement).getPropertyValue('--bg'),
      );
      expect(contrast(foreground, background), theme).toBeGreaterThanOrEqual(4.5);
    }
  });
});

describe('the report-to-Planner entry', () => {
  it('stays visible beside a long title and focuses the supplied Planner', async () => {
    await browserPage.viewport(1200, 800);
    const onReply = vi.fn();
    const start = vi.fn();
    renderPage({ track: track({ title: 'A long-running report '.repeat(20) }), onReply, onStartConversation: start });
    const button = browserPage.getByRole('button', { name: 'Planner', exact: true }).element();
    const box = button.getBoundingClientRect();
    expect(box.width).toBe(28);
    expect(button.querySelector('svg')).not.toBeNull();
    expect(browserPage.getByRole('button', { name: 'Chat', exact: true }).query()).toBeNull();
    expect(box.right).toBeLessThanOrEqual(window.innerWidth);
    await userEvent.click(button);
    expect(onReply).toHaveBeenCalledOnce();
    expect(start).not.toHaveBeenCalled();
  });

  it('keeps failed-task details reachable while the list remains compact', async () => {
    await browserPage.viewport(1200, 800);
    const reason = 'Validation failed because the expected report snapshot is missing. '.repeat(4);
    renderPage({ tasks: [{ blockId: 'verify', key: 'verify', state: 'ready', declaration: null,
      status: 'failed', statusDetail: reason, kind: 'codex', workerCardId: null, pendingReason: null }] });
    const group = document.querySelector<HTMLDetailsElement>('[data-nc-inventory-group="failed"]')!;
    expect(group.open).toBe(true);
    const reveal = group.querySelector<HTMLElement>('[data-nc-row-action="reveal-block"]')!;
    expect(reveal.getAttribute('aria-description')).toContain('Validation failed');
    const metadata = group.querySelector<HTMLElement>('[data-nc-inventory-metadata]')!;
    expect(metadata.getBoundingClientRect().width).toBeLessThanOrEqual(1);
    expect(group.querySelector<HTMLElement>('[data-nc-field="kind"]')!.getBoundingClientRect().right)
      .toBeLessThanOrEqual(group.getBoundingClientRect().right);
  });
});

describe('the Track conversation entry', () => {
  it('offers one visible Chat action on desktop and phone using the same callback', async () => {
    const start = vi.fn();
    await browserPage.viewport(1200, 800);
    renderPage({ onStartConversation: start });
    const chat = browserPage.getByRole('button', { name: 'Chat', exact: true });
    await expect.element(chat).toBeVisible();
    for (const theme of ['light', 'dark']) {
      document.documentElement.dataset.theme = theme;
      const button = [...document.querySelectorAll('button')].find((element) =>
        element.getAttribute('aria-label') === 'Chat' && element.getBoundingClientRect().width > 0);
      if (button === undefined) throw new Error('the desktop Chat action is missing');
      await waitFor(() => {
        const style = getComputedStyle(button);
        expect(contrast(paintedRgb(style.color), paintedRgb(getComputedStyle(button.closest('header')!).backgroundColor))).toBeGreaterThanOrEqual(4.5);
      });
    }
    await userEvent.click(chat);
    expect(start).toHaveBeenCalledTimes(1);

    await browserPage.viewport(390, 844);
    await expect.element(chat).toBeVisible();
    await userEvent.click(chat);
    expect(start).toHaveBeenCalledTimes(2);
  });
});
