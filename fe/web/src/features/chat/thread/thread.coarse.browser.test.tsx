/* The exchange rail as a finger gets it, in a browser context of its own (`vitest.config.ts`): `pointer: coarse` is a
   media feature nothing in a page can set. A tablet, not a phone: a phone width paints no seam at all. */
import { act, cleanup, render } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';
import { commands, userEvent } from 'vitest/browser';

declare module 'vitest/browser' { interface BrowserCommands { tap(selector: string): Promise<void> } }

/* The whole cascade before the component: a CSS Module imported first registers `@layer features` ahead of everything. */
import '../../../styles/entry.css';

import { ChatThread } from './public.tsx';
import type { Conversation, ConversationTurn } from '../../../../../core/domain/conversation.ts';
import drawerStyles from '../../../ui/drawer/drawer.module.css';

afterEach(() => { cleanup(); document.body.replaceChildren(); });

function railConversation(): Conversation {
  return {
    id: 'c1', trackId: 'w1', trackTitle: 'Ship the rewrite', title: null, kind: 'codex',
    state: 'idle', updatedAt: 0, turns: 0,
  };
}

/** `count` exchanges with one-word replies. Nothing here turns on the
 *  transcript's own height — every claim below is about the seam. */
function railTurns(count: number): ConversationTurn[] {
  return Array.from({ length: count }).flatMap((_unused, index) => [
    { id: `you-${index}`, author: 'you' as const, text: `Ask ${index}`, atMs: index * 2_000 },
    { id: `agent-${index}`, author: 'agent' as const, text: 'Short.', atMs: index * 2_000 + 1 },
  ]);
}

/** `--space-9` + `--space-11`, from `.drawer`'s `inset-block`: how much taller than its pane the host has to be for the seam to come out at exactly `paneHeight`. */
const DRAWER_BLOCK_INSETS = 20 + 28;

/** The drawer as four boxes carrying `ui/drawer`'s own classes, so the rail is portalled into a real seam; a hand-rolled pane renders no rail at all. */
function RailPane({ turns, paneHeight = 400 }: {
  turns: readonly ConversationTurn[];
  paneHeight?: number;
}) {
  return (
    <div
      data-nc-rail-host=""
      style={{
        position: 'relative',
        containerType: 'inline-size',
        blockSize: paneHeight + DRAWER_BLOCK_INSETS,
        inlineSize: 380,
        ['--panel-span' as string]: '300px',
      }}
    >
      <div className={drawerStyles.drawer} data-nc-drawer="" style={{ animation: 'none', transition: 'none' }}>
        <div
          className={drawerStyles.scroll}
          data-nc-drawer-scroll=""
          style={{ blockSize: paneHeight, flex: 'none' }}
        >
          <div className={drawerStyles.bodyInner} data-nc-rail-pane-inner="">
            <ChatThread canContinue={false} cards={{}} stalled={false} conversation={railConversation()} turns={turns} />
          </div>
        </div>
      </div>
      <div className={drawerStyles.seam} data-nc-drawer-seam="" style={{ animation: 'none', transition: 'none' }} />
    </div>
  );
}

const railTrack = () => document.querySelector<HTMLElement>('[data-nc-rail-track]')!;
const dots = () => [...document.querySelectorAll<HTMLElement>('button[aria-label^="Jump to "]')];
const railPreview = () => document.querySelector<HTMLElement>('[data-nc-rail-preview]');

async function frame() {
  await act(async () => {
    await new Promise((resolve) => { requestAnimationFrame(() => { resolve(null); }); });
  });
}

/** Two frames: one for the scroll handler's own rAF, one for the render it
 *  schedules. */
async function settle() {
  await frame();
  await frame();
}

/** Wait out a real interval inside `act`, so a `setTimeout` that lands in
 *  component state is flushed rather than warned about. */
async function pause(ms: number) {
  await act(async () => { await new Promise((resolve) => { setTimeout(resolve, ms); }); });
}

/** The painted diameter of a dot's ink — the `::before`, not the button. */
function dotInk(index: number): number {
  return Number.parseFloat(getComputedStyle(dots()[index], '::before').width);
}

/** The index of the dot the rail has lit, so a "resting" size is never read off
 *  it by accident: the lit dot rests at `--nc-rail-dot-current`. */
function litDot(): number {
  return dots().findIndex((dot) => dot.getAttribute('aria-current') === 'true');
}

function centre(index: number): number {
  const box = dots()[index].getBoundingClientRect();
  return box.top + box.height / 2;
}

describe('the exchange rail on a coarse pointer, as the engine lays it out', () => {
  /* Every other case is a statement about a device, so the queries are read off the engine first. `pointer: none` is the state CDP touch emulation leaves behind; the inner box is Vitest's iframe (`browser.viewport`), which is what `@media (width < 60rem)` is evaluated against. */
  it('runs in a context that reports a coarse pointer and nothing else', () => {
    expect(matchMedia('(pointer: coarse)').matches).toBe(true);
    expect(matchMedia('(pointer: fine)').matches).toBe(false);
    expect(matchMedia('(pointer: none)').matches).toBe(false);
    expect(matchMedia('(any-pointer: coarse)').matches).toBe(true);
    expect(screen.width).toBe(1024);
    expect(screen.height).toBe(1366);
    expect(navigator.maxTouchPoints).toBeGreaterThan(0);
    expect('orientation' in window).toBe(true);
    expect(screen.orientation.type).toBe('portrait-primary');
    /* Coarse and wide: the seam the rail mounts in exists only above the 60rem breakpoint. */
    expect(matchMedia('(width >= 60rem)').matches).toBe(true);
    expect(window.innerWidth).toBeGreaterThanOrEqual(960);
  });

  /* Touch rows stay at a full 44px pitch, including the first and last targets. */
  it('lays out a 24 by 44 target at a flat 44px pitch, with no shoulders', async () => {
    render(<RailPane turns={railTurns(8)} />);
    await settle();

    const lit = litDot();
    expect(lit).toBeGreaterThanOrEqual(0);
    /* A dot that is not the lit one, so what is measured is the rest state. */
    const resting = lit === 1 ? 2 : 1;
    const box = dots()[resting].getBoundingClientRect();

    expect(box.height).toBe(44);
    expect(box.width).toBe(24);
    expect(box.height).toBeGreaterThanOrEqual(24);
    expect(box.width).toBeGreaterThanOrEqual(24);

    expect(centre(2) - centre(1)).toBe(44);
    expect(centre(3) - centre(2)).toBe(44);

    expect(dotInk(resting)).toBe(6);
    await expect.poll(() => dotInk(lit)).toBe(8);

    /* Every row is a direct stable target; the shared layer sits outside the scroll track. */
    expect(railTrack().firstElementChild).toBe(dots()[0]);
    expect(railTrack().querySelectorAll('button').length).toBe(dots().length);

    expect(getComputedStyle(dots()[0]).marginBlockStart).toBe('0px');
    expect(getComputedStyle(dots().at(-1)!).marginBlockEnd).toBe('0px');
  });

  /* Touch activation skips the layer; keyboard focus on a tablet still gets a visible preview. */
  it('keeps touch jumps direct and shows a usable preview for keyboard navigation', async () => {
    render(<RailPane turns={railTurns(8)} />);
    await settle();
    await commands.tap('button[aria-label="Jump to exchange 4: Ask 3"]');
    await pause(600);
    expect(railPreview()).toBeNull();
    expect(dots().every(dot => dot.style.getPropertyValue('--nc-dot-proximity') === '')).toBe(true);
    dots().forEach((dot, index) => {
      if (dot.getAttribute('aria-current') !== 'true') expect(dotInk(index)).toBe(6);
    });
    await userEvent.keyboard('{ArrowUp}');
    await settle();
    const preview = railPreview();
    expect(preview).not.toBeNull();
    expect(getComputedStyle(preview!.closest('[popover]')!).display).not.toBe('none');
    expect(preview!.getBoundingClientRect().height).toBeGreaterThan(0);
    await userEvent.keyboard('{Escape}');
    await settle();
    expect(railPreview()).toBeNull();
  });

  /* Under a finger the 320px cap is reached at eight exchanges (7 → 308px, 8 → 352px); the fine branch reaches it at seventeen. The cap is read off the engine. */
  it('overflows the 320px cap at eight exchanges, and stays reachable past it', async () => {
    render(<RailPane turns={railTurns(7)} paneHeight={700} />);
    await settle();

    const cap = Number.parseFloat(getComputedStyle(railTrack()).maxBlockSize);
    expect(cap).toBe(320);
    expect(railTrack().clientHeight).toBe(308);
    /* Read off the column rather than `scrollHeight`, which is floored at `clientHeight` and would be 320 at one row too. */
    const column = dots().at(-1)!.getBoundingClientRect().bottom - dots()[0].getBoundingClientRect().top;
    expect(column).toBe(308);
    expect(railTrack().scrollHeight).toBe(308);

    cleanup(); document.body.replaceChildren();
    render(<RailPane turns={railTurns(8)} paneHeight={700} />);
    await settle();

    /* Twelve is 336, and the twelfth row is the one that crosses. */
    expect(railTrack().scrollHeight).toBe(352);
    expect(railTrack().scrollHeight).toBeGreaterThan(railTrack().clientHeight);

    const track = railTrack();
    track.scrollTop = 0;
    await settle();
    expect(dots()[0].getBoundingClientRect().top)
      .toBeGreaterThanOrEqual(track.getBoundingClientRect().top - 0.5);

    track.scrollTop = track.scrollHeight - track.clientHeight;
    await settle();
    expect(track.scrollTop).toBe(32);
    expect(dots()[7].getBoundingClientRect().bottom)
      .toBeLessThanOrEqual(track.getBoundingClientRect().bottom + 0.5);
  });
});
