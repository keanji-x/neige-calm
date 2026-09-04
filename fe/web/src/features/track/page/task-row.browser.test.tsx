/*
 * The TASKS row's geometry, and the one thing about it jsdom cannot answer.
 *
 * The row carries two controls and a dot (#1149): the row reveals the block,
 * the worker kind opens the worker card, and the status dot is a named graphic
 * at the trailing edge. The dot is `position: absolute` inside the reveal
 * button — DOM-inside so a click on it is a click on the row's own action,
 * visually outside the flow so it can sit past the kind. That arrangement is
 * *entirely* a layout claim: jsdom stores `position` and `inset-inline-end` as
 * strings, returns zeroes from every rect, and would report the same tree
 * whether the dot landed at the row's trailing edge, under the key, or off the
 * card altogether.
 *
 * So this asserts what a pointer would hit, with an engine:
 *   - the dot is inside the row and at its trailing end, after the kind;
 *   - a click in the row's open middle lands on the reveal control;
 *   - a click on the dot also lands on the reveal control — the trailing corner
 *     is not a dead zone;
 *   - a click on the kind lands on the kind, which is the only place the card
 *     is reachable from.
 *
 * And the second half of the file asks the engine the other thing jsdom cannot
 * answer: **what the four statuses look like.** The dot used to be separated by
 * hue alone, which three of the four states could not survive — measured off
 * the shipped tokens, no two status fills reach even 1.6:1 against each other,
 * and in dark `--success` and `--error` are the same lightness to the decimal.
 * So the dot now carries a second, independent channel, form, and "form" is
 * precisely the kind of claim a class-name assertion cannot make: a test that
 * checks `className` passes whether or not a single rule ever matched. These
 * read `background-color`, `border-width`, `border-radius` and `padding` back
 * out of the cascade instead, measure the rendered rects, and paint the fills
 * onto a canvas to measure the separation the colour channel actually
 * delivers.
 */
import { render, waitFor } from '@testing-library/react';
import { commands, page as browserPage, userEvent } from 'vitest/browser';
import { afterEach, beforeAll, describe, expect, it, vi } from 'vitest';

/*
 * `prefers-reduced-motion` is a media *feature*: nothing in the page can set
 * one, and a test that reads the rule's own text back out of the stylesheet
 * proves only that somebody typed it. The driver can emulate it, so it is
 * exposed as a browser command — see `vitest.config.ts`.
 */
declare module 'vitest/internal/browser' {
  interface BrowserCommands {
    emulateReducedMotion: (reduce: boolean) => Promise<void>;
  }
}

/* The whole cascade, and before the CSS Module — see the import-order note in
   `panel-sticky.browser.test.tsx`. */
import '../../../styles/entry.css';

import type { ReportTaskRow } from '../../../../../core/domain/report.ts';
import { NEUTRAL_ACTIVITY, type Track } from '../../../../../core/domain/track.ts';
import { TrackPage } from './public.tsx';

afterEach(async () => {
  document.body.replaceChildren();
  delete document.documentElement.dataset.theme;
  /* Media emulation outlives the test that set it — the page is one browser
     tab for the whole file — so it is unwound here rather than at the end of
     the test that needs it, which a failure would skip. */
  await commands.emulateReducedMotion(false);
});

const track: Track = {
  id: 'w1', areaId: 'c1', title: 'Alpha', sort: 1, lifecycle: 'working', cwd: '/tmp/alpha',
  archivedAt: null, pinnedAt: null, terminalAt: null, createdAt: 0, updatedAt: 0,
  ...NEUTRAL_ACTIVITY,
};

const assigned: ReportTaskRow = {
  blockId: 'b-bench', key: 'bench-harness', state: 'ready',
  declaration: null, status: 'running', statusDetail: null, kind: 'terminal', workerCardId: 'c-4', pendingReason: null,
};

function renderRow(onOpenCard: () => void, onOpenTask: () => void) {
  render(
    <div style={{ inlineSize: 1200, blockSize: 800 }}>
      <TrackPage
        track={track}
        cards={[]}
        tasks={[assigned]}
        onOpenCard={onOpenCard}
        onOpenTask={onOpenTask}
        onRenameTrack={vi.fn()}
        onDeleteTrack={vi.fn()}
      />
    </div>,
  );
  const row = document.querySelector<HTMLElement>('[data-nc-task-inventory] li')!;
  const dot = row.querySelector<HTMLElement>('[data-nc-status]')!;
  const kind = row.querySelector<HTMLElement>('button[title^="Open the worker card"]')!;
  const reveal = row.querySelector<HTMLElement>('button[title^="Show "]')!;
  return { row, dot, kind, reveal };
}

/** What a pointer at this point would activate: the nearest button ancestor. */
function controlAt(x: number, y: number): Element | null {
  return document.elementFromPoint(x, y)?.closest('button') ?? null;
}

describe('a TASKS row, laid out', () => {
  it('puts the status dot at the trailing edge, after the kind, inside the row', async () => {
    await browserPage.viewport(1200, 800);
    const { row, dot, kind } = renderRow(vi.fn(), vi.fn());

    const rowBox = row.getBoundingClientRect();
    const dotBox = dot.getBoundingClientRect();
    const kindBox = kind.getBoundingClientRect();

    /* Premise: these boxes are real. A zero-size rect would make every
       comparison below vacuously true, which is exactly what jsdom returns. */
    expect(rowBox.width).toBeGreaterThan(100);
    expect(dotBox.width).toBeGreaterThan(0);

    expect(dotBox.left).toBeGreaterThan(kindBox.right);
    expect(dotBox.right).toBeLessThanOrEqual(Math.ceil(rowBox.right));
    /* Vertically centred on the row, within a pixel of rounding. */
    expect(Math.abs((dotBox.top + dotBox.height / 2) - (rowBox.top + rowBox.height / 2)))
      .toBeLessThanOrEqual(1);
  });

  it('keeps a pending reason out of the row and reveals compact copy only on hover', async () => {
    await browserPage.viewport(1200, 800);
    const message = 'Queued 1/1';
    render(
      <div style={{ inlineSize: 1200, blockSize: 800 }}>
        <TrackPage
          track={track}
          cards={[]}
          tasks={[{
            ...assigned,
            status: 'pending',
            kind: 'codex',
            workerCardId: null,
            pendingReason: {
              kind: 'budgetQueued', message, occupiedTaskBudget: 1, effectiveTaskBudget: 1,
            },
          }]}
          onOpenCard={vi.fn()}
          onOpenTask={vi.fn()}
          onRenameTrack={vi.fn()}
          onDeleteTrack={vi.fn()}
        />
      </div>,
    );
    const row = document.querySelector<HTMLElement>('[data-nc-task-inventory] li')!;
    const key = row.querySelector<HTMLElement>('[data-nc-field="title"]')!;
    const reason = row.querySelector<HTMLElement>('[data-nc-badge="pending-reason:budgetQueued"]')!;
    const reveal = row.querySelector<HTMLElement>('button[title^="Show "]')!;
    const layer = reason.closest<HTMLElement>('[popover]')!;

    expect(row.innerText).not.toContain(message);
    expect(key.innerText).toBe('bench-harness');
    expect(layer.matches(':popover-open')).toBe(false);

    await userEvent.hover(reveal);
    await waitFor(() => { expect(layer.matches(':popover-open')).toBe(true); }, { timeout: 2000 });
    expect(reason.innerText).toBe(message);
    expect(getComputedStyle(reason).fontSize).toBe('11px');
  });

  it('gives the row, its open middle and its dot to the reveal control, and only the kind to the card', async () => {
    await browserPage.viewport(1200, 800);
    const onOpenCard = vi.fn();
    const onOpenTask = vi.fn();
    const { row, dot, kind, reveal } = renderRow(onOpenCard, onOpenTask);

    const rowBox = row.getBoundingClientRect();
    const kindBox = kind.getBoundingClientRect();
    const dotBox = dot.getBoundingClientRect();
    const middle = rowBox.top + rowBox.height / 2;

    /* The empty run between the key and the kind — the part of the row a reader
       aims at when they mean "this task". */
    const gap = (kindBox.left + rowBox.left) / 2;
    expect(gap).toBeLessThan(kindBox.left);
    expect(controlAt(gap, middle)).toBe(reveal);
    /* The trailing corner: painted past the kind, still the row's own action. */
    expect(controlAt(dotBox.left + dotBox.width / 2, middle)).toBe(reveal);
    expect(controlAt(kindBox.left + kindBox.width / 2, middle)).toBe(kind);

    /* And the hits really are the two different callbacks. */
    (controlAt(gap, middle) as HTMLElement).click();
    expect(onOpenTask).toHaveBeenCalledWith('b-bench');
    expect(onOpenCard).not.toHaveBeenCalled();
    (controlAt(kindBox.left + kindBox.width / 2, middle) as HTMLElement).click();
    expect(onOpenCard).toHaveBeenCalledWith('c-4');
    expect(onOpenTask).toHaveBeenCalledTimes(1);
  });

  /*
   * ── The two dead zones ───────────────────────────────────────────────────
   *
   * Both were in the ordinary row, not at an edge, and both contradicted the
   * one thing the row promises. They are hit-tested rather than asserted about
   * the DOM because the fix is a layout fact (`.taskReveal::before` covers the
   * whole `<li>`; the kind's `<span>` form is left unpositioned so the sheet
   * lies over it) and jsdom would report the same tree either way — which is
   * how a jsdom test came to enshrine the first of them as intended.
   */
  it('gives the row its own action where the kind is a label and not a control', async () => {
    await browserPage.viewport(1200, 800);
    const onOpenCard = vi.fn();
    const onOpenTask = vi.fn();
    /* A codex row as it ships today: `app/router` cleared `workerCardId`
       because the registry has no adapter to draw that card. */
    render(
      <div style={{ inlineSize: 1200, blockSize: 800 }}>
        <TrackPage
          track={track}
          cards={[]}
          tasks={[{ ...assigned, kind: 'codex', workerCardId: null }]}
          onOpenCard={onOpenCard}
          onOpenTask={onOpenTask}
          onRenameTrack={vi.fn()}
          onDeleteTrack={vi.fn()}
        />
      </div>,
    );
    const row = document.querySelector<HTMLElement>('[data-nc-task-inventory] li')!;
    const reveal = row.querySelector<HTMLElement>('button[title^="Show "]')!;
    /* Premise: the kind really is a label here — a `<button>` would make the
       hit below trivially true for the wrong reason. */
    expect(row.querySelector('button[title^="Open the worker card"]')).toBeNull();
    const label = [...row.querySelectorAll<HTMLElement>('span')]
      .find((span) => span.textContent === 'codex')!;
    const box = label.getBoundingClientRect();
    expect(box.width).toBeGreaterThan(0);

    const hit = document.elementFromPoint(box.left + box.width / 2, box.top + box.height / 2);
    expect((hit as HTMLElement).closest('button')).toBe(reveal);
    (hit as HTMLElement).click();
    expect(onOpenTask).toHaveBeenCalledWith('b-bench');
    expect(onOpenCard).not.toHaveBeenCalled();
  });

  /*
   * The other one: the row reserves the trailing lane unconditionally, so the
   * kind column stays aligned as tasks are dispatched — but only the dot's own
   * target ever filled it. An undispatched row therefore had a 24px hole at the
   * corner the eye is drawn to, and that is the most common row in a track that
   * has just been planned.
   */
  it('gives the reserved trailing lane to the reveal control on a row with no status dot', async () => {
    await browserPage.viewport(1200, 800);
    const onOpenCard = vi.fn();
    const onOpenTask = vi.fn();
    render(
      <div style={{ inlineSize: 1200, blockSize: 800 }}>
        <TrackPage
          track={track}
          cards={[]}
          tasks={[{ ...assigned, status: null, declaration: 'Not ready' }]}
          onOpenCard={onOpenCard}
          onOpenTask={onOpenTask}
          onRenameTrack={vi.fn()}
          onDeleteTrack={vi.fn()}
        />
      </div>,
    );
    const row = document.querySelector<HTMLElement>('[data-nc-task-inventory] li')!;
    const reveal = row.querySelector<HTMLElement>('button[title^="Show "]')!;
    const kind = row.querySelector<HTMLElement>('button[title^="Open the worker card"]')!;
    /* Premise: there really is no dot, and the lane is still reserved. */
    expect(row.querySelector('[data-nc-status]')).toBeNull();
    const rowBox = row.getBoundingClientRect();
    const kindBox = kind.getBoundingClientRect();
    const lane = rowBox.right - kindBox.right;
    expect(lane).toBeGreaterThan(8);

    const middle = rowBox.top + rowBox.height / 2;
    for (const x of [kindBox.right + lane / 2, rowBox.right - 2]) {
      expect(controlAt(x, middle)).toBe(reveal);
    }
    (controlAt(rowBox.right - 2, middle) as HTMLElement).click();
    expect(onOpenTask).toHaveBeenCalledWith('b-bench');
    expect(onOpenCard).not.toHaveBeenCalled();
  });

  /*
   * ── The highlight is for keyboard focus, not for having been clicked ─────
   *
   * The row is an `<li>`, so `:focus-visible` on it (what `.cardRow` uses)
   * cannot match — and the rule became `:focus-within`, which fires on pointer
   * focus. A click then left `--overlay-hover` painted on a row the pointer had
   * long since left, and the two row types in one panel card disagreed about
   * what a highlight means. `:has(:focus-visible)` is the row asking for what
   * `.cardRow` asks for.
   *
   * Only measurable in an engine: `:focus-visible` is the browser's own
   * heuristic about how focus arrived, and jsdom has neither the heuristic nor
   * the resolved background.
   */
  it('highlights the row for keyboard focus and not for a pointer click', async () => {
    await browserPage.viewport(1200, 800);
    const { row, reveal } = renderRow(vi.fn(), vi.fn());
    const background = () => getComputedStyle(row).backgroundColor;
    /* Premise: nothing is hovered or focused yet, so this is the resting
       colour every comparison below is against. */
    expect(row.matches(':hover')).toBe(false);
    const resting = background();

    /*
     * Pointer first, and the order matters: Chromium decides `:focus-visible`
     * from the modality of the interaction that moved focus, so clicking a
     * control the keyboard had *already* focused leaves the previous verdict
     * standing. Clicking it cold is the case a reader lives in.
     */
    await userEvent.click(reveal);
    await userEvent.unhover(row);
    expect(row.matches(':hover')).toBe(false);
    /* The row really does contain the focus — which is exactly what
       `:focus-within` was painting, and what must no longer show. */
    expect(row.matches(':focus-within')).toBe(true);
    expect(background()).toBe(resting);

    /* Keyboard: leave, then tab back in with real Tab presses — the input
       `:focus-visible` is a judgement about. */
    (document.activeElement as HTMLElement | null)?.blur();
    for (let i = 0; i < 40 && document.activeElement !== reveal; i += 1) {
      await userEvent.tab();
    }
    expect(document.activeElement).toBe(reveal);
    expect(row.matches(':hover')).toBe(false);
    expect(background()).not.toBe(resting);
  });
});

/*
 * ── The status mark: form first, colour second ──────────────────────────────
 */

/** One representative of each of the four marks the panel can paint. */
const MARKS = [
  { status: 'running', mark: 'bullseye' },
  { status: 'done', mark: 'filled disc' },
  { status: 'failed', mark: 'filled square' },
  { status: 'pending', mark: 'hollow ring' },
] as const;

function renderMarks(): Map<string, HTMLElement> {
  render(
    <div style={{ inlineSize: 1200, blockSize: 800 }}>
      <TrackPage
        track={track}
        cards={[]}
        tasks={MARKS.map(({ status }, index): ReportTaskRow => ({
          blockId: `b-${index}`,
          key: `task-${index}`,
          state: 'ready',
          declaration: null,
          status,
          statusDetail: null,
          kind: 'terminal',
          workerCardId: `c-${index}`,
          pendingReason: null,
        }))}
        onOpenCard={vi.fn()}
        onOpenTask={vi.fn()}
        onRenameTrack={vi.fn()}
        onDeleteTrack={vi.fn()}
      />
    </div>,
  );
  /* One static selector, then keyed by the attribute the row already carries —
     a per-status selector string would be a dynamic query, which this repo
     forbids because it fails open when the value stops matching. */
  const dots = new Map(
    [...document.querySelectorAll<HTMLElement>('[data-nc-status]')]
      .map((dot) => [dot.dataset.ncStatus ?? '', dot] as const),
  );
  /* Premise: all four really rendered. A missing one would make every loop
     below run over three elements and still pass. */
  expect([...dots.keys()].sort()).toEqual(MARKS.map(({ status }) => status).toSorted());
  return dots;
}

/*
 * The same measurement `styles/contrast-matrix.browser.test.ts` makes and for
 * the same reason: Chromium hands `getComputedStyle` back the *authored* colour
 * space, so reading the string tells us nothing about the pixel. Painting it
 * runs the compositor's own parse and out-of-gamut mapping.
 */
let ctx: CanvasRenderingContext2D;
beforeAll(() => {
  const canvas = document.createElement('canvas');
  canvas.width = 1;
  canvas.height = 1;
  const got = canvas.getContext('2d', { willReadFrequently: true });
  if (!got) throw new Error('no 2d context');
  ctx = got;
});

/** sRGB bytes plus alpha, as the screen would receive them. */
function paint(cssColor: string): [number, number, number, number] {
  ctx.clearRect(0, 0, 1, 1);
  ctx.fillStyle = cssColor;
  ctx.fillRect(0, 0, 1, 1);
  const [r, g, b, a] = ctx.getImageData(0, 0, 1, 1).data;
  return [r, g, b, a];
}

const channel = (v: number) => {
  const c = v / 255;
  return c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
};
const luminance = ([r, g, b]: readonly number[]) =>
  0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b);
function contrast(a: readonly number[], b: readonly number[]) {
  const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x);
  return (hi + 0.05) / (lo + 0.05);
}
/** OKLCH lightness, the unit the surface ladder is specified in (§6.5). */
function lightness([R, G, B]: readonly number[]): number {
  const [r, g, b] = [R, G, B].map(channel);
  const l = Math.cbrt(0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b);
  const m = Math.cbrt(0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b);
  const s = Math.cbrt(0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b);
  return (0.2104542553 * l + 0.793617785 * m - 0.0040720468 * s) * 100;
}

/** The colour a mark paints: its fill, or its stroke when it has no fill. */
function markColour(dot: HTMLElement): [number, number, number, number] {
  const style = getComputedStyle(dot);
  const fill = paint(style.backgroundColor);
  return fill[3] > 0 ? fill : paint(style.borderTopColor);
}

/** Every rule the document has, `@media` and `@keyframes` blocks flattened. */
function* flatten(rules: CSSRuleList): Generator<CSSRule> {
  for (const rule of Array.from(rules)) {
    yield rule;
    const nested = (rule as CSSGroupingRule).cssRules;
    if (nested) yield* flatten(nested);
  }
}
function documentRules(): CSSRule[] {
  return Array.from(document.styleSheets).flatMap((sheet) => {
    try { return Array.from(flatten(sheet.cssRules)); } catch { return []; }
  });
}

/**
 * The dimmest frame of the in-flight pulse, read out of the live cascade.
 *
 * Keyed on the *computed* animation name rather than the authored one: this is
 * a CSS Module, so `task-dot-pulse` reaches the document as
 * `_task-dot-pulse_1uize_1` and a literal would find nothing.
 */
function pulseFloor(animationName: string): number {
  const keyframes = documentRules().find(
    (rule): rule is CSSKeyframesRule =>
      rule instanceof CSSKeyframesRule && rule.name === animationName,
  );
  if (!keyframes) throw new Error(`no ${animationName} keyframes in the document`);
  /* The implicit 0%/100% frames are the element's own `opacity: 1`. */
  return Math.min(1, ...Array.from(keyframes.cssRules).map(
    (frame) => Number.parseFloat((frame as CSSKeyframeRule).style.opacity || '1'),
  ));
}

describe('the TASKS status mark', () => {
  it('is the --dot-md rung, not the smallest one the scale has', async () => {
    await browserPage.viewport(1200, 800);
    const dots = renderMarks();
    const root = getComputedStyle(document.documentElement);
    const md = root.getPropertyValue('--dot-md').trim();
    const sm = root.getPropertyValue('--dot-sm').trim();

    /* Premise: the two rungs are different, or "not the small one" says
       nothing. */
    expect(md).not.toBe(sm);
    for (const { status } of MARKS) {
      const style = getComputedStyle(dots.get(status)!);
      expect([status, style.width, style.height]).toEqual([status, md, md]);
    }
  });

  /*
   * **Every mark is the same size on the glass, ring included.** The first cut
   * hung a detached `outline` off the in-flight dot, and its *rendered* box was
   * 14px in a column of 8px ones — a difference `getComputedStyle().width`
   * cannot see, because `width` is the border box and an outline is painted
   * outside it. So this measures `getBoundingClientRect()`, and separately
   * forbids anything painting beyond that rect at all: an outline, a spread
   * shadow, or a `transform: scale` would each restore the same defect while
   * every declared width stayed 8px.
   */
  it('renders all four at one diameter, with nothing painted outside the box', async () => {
    await browserPage.viewport(1200, 800);
    const dots = renderMarks();
    const boxes = MARKS.map(({ status }) => dots.get(status)!.getBoundingClientRect());

    /* Premise: the rects are real. jsdom answers zero to all of this. */
    expect(boxes[0].width).toBeGreaterThan(0);
    expect(new Set(boxes.map(({ width, height }) => `${width}x${height}`)).size).toBe(1);

    for (const { status } of MARKS) {
      const style = getComputedStyle(dots.get(status)!);
      expect([status, style.outlineStyle]).toEqual([status, 'none']);
      /* No spread ring either — `box-shadow: none` is the whole claim, since
         any shadow with spread paints outside the measured rect. */
      expect([status, style.boxShadow]).toEqual([status, 'none']);
    }
  });

  /*
   * The row's trailing cluster, at the width the panel actually is.
   *
   * The TASKS panel is a *fixed-width* column (`--panel-w`, 280px) whatever the
   * window does, so the panel's own narrow case is the only case: this renders
   * at the ordinary viewport and gets the production width. The key is
   * `overflow: hidden`, so a long one runs to an ellipsis and stops one gap
   * short of the kind. That gap and the
   * kind → dot gap are the two distances the reader reads as "these are
   * separate things"; at `--space-2` they were not enough and the row read as
   * crowded. This measures both off the rendered rects, and checks the key is
   * genuinely truncated first — against an untruncated key the gaps would be
   * whatever the leftover happened to be and the test would prove nothing.
   */
  it('keeps a full step between the key, the kind and the dot when the key truncates', async () => {
    await browserPage.viewport(1200, 800);
    const step = Number.parseFloat(
      getComputedStyle(document.documentElement).getPropertyValue('--space-4'),
    );
    render(
      <div style={{ inlineSize: 1200, blockSize: 800 }}>
        <TrackPage
          track={track}
          cards={[]}
          tasks={[{
            ...assigned,
            key: 'a-deliberately-very-long-task-key-that-cannot-possibly-fit-the-panel',
          }]}
          onOpenCard={vi.fn()}
          onOpenTask={vi.fn()}
          onRenameTrack={vi.fn()}
          onDeleteTrack={vi.fn()}
        />
      </div>,
    );
    const row = document.querySelector<HTMLElement>('[data-nc-task-inventory] li')!;
    /* The reveal button's first child is the key — see `public.tsx`. A class
       selector would be a dynamic query, which this repo forbids. */
    const key = row.querySelector<HTMLElement>('button[title^="Show "]')!
      .firstElementChild as HTMLElement;
    const kind = row.querySelector<HTMLElement>('button[title^="Open the worker card"]')!;
    const dot = row.querySelector<HTMLElement>('[data-nc-status]')!;

    /* Premise: the key really is clipped to an ellipsis here. */
    expect(key.scrollWidth).toBeGreaterThan(key.clientWidth);

    const [keyBox, kindBox, dotBox] = [key, kind, dot].map((el) => el.getBoundingClientRect());
    expect(kindBox.left - keyBox.right).toBeGreaterThanOrEqual(step);
    expect(dotBox.left - kindBox.right).toBeGreaterThanOrEqual(step);
    /* And the dot still fits the lane the row reserves for it. */
    expect(dotBox.right).toBeLessThanOrEqual(Math.ceil(row.getBoundingClientRect().right));
  });

  /*
   * Vertically, the dot lines up with the words beside it — measured against
   * the kind's *text*, not its button box. The button is `align-self: stretch`,
   * so its rect is the row's full height and centring on it would be true
   * however the type sat inside it; a `Range` over the text node is the line
   * box the reader actually sees.
   */
  it('sits on the middle of the kind text, not merely of the row', async () => {
    await browserPage.viewport(1200, 800);
    const dots = renderMarks();
    const range = document.createRange();

    for (const { status } of MARKS) {
      const dot = dots.get(status)!;
      /* Each dot against the text on *its own* row. */
      const row = dot.closest('li')!;
      range.selectNodeContents(row.querySelector('button[title^="Open the worker card"]')!);
      const text = range.getBoundingClientRect();
      /* Premise: a real line box, and a shorter one than the row — otherwise
         "centred on the text" and "centred on the row" are the same claim. */
      expect([status, text.height > 0]).toEqual([status, true]);
      expect([status, text.height < row.getBoundingClientRect().height])
        .toEqual([status, true]);

      const box = dot.getBoundingClientRect();
      const offset = Math.abs((box.top + box.height / 2) - (text.top + text.height / 2));
      expect([status, offset <= 1]).toEqual([status, true]);
    }
  });

  /*
   * **The form channel, with the colour taken away.** The signature below is
   * built only out of geometry — is there a fill at all, what shape are the
   * corners, how thick is the stroke, is there a gap between stroke and fill —
   * so four distinct signatures is the
   * claim that a reader who cannot tell the hues apart can still tell the
   * states apart. Collapse the four rules onto one shape and this is the
   * assertion that reddens; nothing about a class name would have moved.
   */
  it('paints four forms that stay distinct with the colour removed', async () => {
    await browserPage.viewport(1200, 800);
    const dots = renderMarks();

    const signature = (dot: HTMLElement) => {
      const style = getComputedStyle(dot);
      return [
        paint(style.backgroundColor)[3] > 0 ? 'filled' : 'hollow',
        style.borderTopLeftRadius,
        style.borderTopWidth,
        style.paddingTop,
        style.backgroundClip,
      ].join('/');
    };
    const signatures = MARKS.map(({ status }) => signature(dots.get(status)!));
    expect(new Set(signatures).size).toBe(MARKS.length);
  });

  it('draws each form the way its state is described', async () => {
    await browserPage.viewport(1200, 800);
    const dots = renderMarks();
    const box = Number.parseFloat(
      getComputedStyle(document.documentElement).getPropertyValue('--dot-md'),
    );

    /* Hollow: no fill at all, and a stroke thick enough to be a stroke. */
    const neutral = getComputedStyle(dots.get('pending')!);
    expect(paint(neutral.backgroundColor)[3]).toBe(0);
    expect(Number.parseFloat(neutral.borderTopWidth)).toBeGreaterThanOrEqual(2);
    /* ...and a hole left over inside it. */
    expect(box - 2 * Number.parseFloat(neutral.borderTopWidth)).toBeGreaterThanOrEqual(4);

    /* Filled disc: opaque, and round — the radius is at least half the box. */
    const done = getComputedStyle(dots.get('done')!);
    expect(paint(done.backgroundColor)[3]).toBe(255);
    expect(Number.parseFloat(done.borderTopLeftRadius)).toBeGreaterThanOrEqual(box / 2);
    expect(done.outlineStyle).toBe('none');

    /* Filled square: opaque, and a corner well short of the circle it would be
       at half the box — the one non-circular silhouette in the panel. */
    const failed = getComputedStyle(dots.get('failed')!);
    expect(paint(failed.backgroundColor)[3]).toBe(255);
    expect(Number.parseFloat(failed.borderTopLeftRadius)).toBeLessThanOrEqual(box / 4);

    /* Bullseye: a ring on the border, a transparent gap on the padding, and a
       core clipped to the content box — all three inside the same 8px. */
    const running = getComputedStyle(dots.get('running')!);
    expect(paint(running.backgroundColor)[3]).toBe(255);
    expect(running.backgroundClip).toBe('content-box');
    const ring = Number.parseFloat(running.borderTopWidth);
    const gap = Number.parseFloat(running.paddingTop);
    expect(ring).toBeGreaterThan(0);
    expect(gap).toBeGreaterThan(0);
    /* The core is what is left, and it has to be left: ring + gap may not eat
       the box. */
    expect(box - 2 * (ring + gap)).toBeGreaterThanOrEqual(box / 2);
    /* The ring is the same fill as the core, so it needs no contrast recipe of
       its own — see `tools/styles/check-contrast.mjs`. */
    expect(paint(running.borderTopColor)).toEqual(paint(running.backgroundColor));
  });

  /*
   * The colour channel, measured rather than asserted about. Each fill has to
   * clear the non-text floor against the card it is painted on — that is what
   * makes it a mark at all — while *no pair of them* clears it against each
   * other. The second half is the whole reason the form channel above exists,
   * and it is written as an assertion so the claim cannot quietly rot: if the
   * palette is ever re-spaced so two statuses do separate, this reddens and the
   * note in `page.module.css` gets re-read instead of inherited.
   */
  it.each(['light', 'dark'] as const)(
    '%s: every fill is legible on the card, and none is legible against another',
    async (theme) => {
      await browserPage.viewport(1200, 800);
      if (theme === 'dark') document.documentElement.dataset.theme = 'dark';
      const dots = renderMarks();
      const card = paint(
        getComputedStyle(document.documentElement).getPropertyValue('--surface-card'),
      );
      /*
       * Premise: the theme really switched. The assertion here used to be
       * `card[3] === 255` — the alpha channel, which is opaque in both themes
       * and so was true whether or not the switch took. The card's *lightness*
       * is the thing that differs, and by most of the scale: the light card is
       * nearly white and the dark one is near the bottom of the ladder.
       */
      expect(card[3]).toBe(255);
      expect([theme, lightness(card) > 50]).toEqual([theme, theme === 'light']);

      const colours = MARKS.map(({ status, mark }) => ({
        status, mark, rgb: markColour(dots.get(status)!),
      }));
      for (const { status, rgb } of colours) {
        expect([status, contrast(rgb, card) >= 3]).toEqual([status, true]);
      }

      const pairs = colours.flatMap((a, i) => colours.slice(i + 1).map((b) => ({
        pair: `${a.status}~${b.status}`,
        ratio: contrast(a.rgb, b.rgb),
        deltaL: Math.abs(lightness(a.rgb) - lightness(b.rgb)),
      })));
      expect(pairs).toHaveLength(6);
      /* Not one of the six is a 3:1 separation; the closest pair in dark is
         done~failed, which is ~0 ΔL apart — hue and nothing else. */
      expect(pairs.filter(({ ratio }) => ratio >= 3)).toEqual([]);
      expect(Math.min(...pairs.map(({ deltaL }) => deltaL))).toBeLessThan(3);
    },
  );

  /*
   * Motion is the fifth thing the running row says, and it is allowed to be
   * suppressed — so it may not be the thing that *makes* the mark. The
   * keyframes are therefore asserted to touch `opacity` and nothing else: an
   * animation that grew a ring, or swapped a radius, would make the in-flight
   * dot periodically the wrong size or the wrong shape, which are exactly the
   * two things this rule was rewritten to stop, and would take that difference
   * away entirely under `prefers-reduced-motion`.
   */
  it('animates only opacity — never the form or the box — and stops even that under reduced motion', async () => {
    await browserPage.viewport(1200, 800);
    const dots = renderMarks();
    const running = dots.get('running')!;
    const name = getComputedStyle(running).animationName;
    expect(name).not.toBe('none');

    const keyframes = documentRules().find(
      (rule): rule is CSSKeyframesRule =>
        rule instanceof CSSKeyframesRule && rule.name === name,
    );
    expect(keyframes).toBeDefined();
    const touched = new Set<string>();
    for (const frame of Array.from(keyframes!.cssRules)) {
      for (const property of Array.from((frame as CSSKeyframeRule).style)) touched.add(property);
    }
    /* Opacity and nothing else: every other animatable property on this element
       is either the form or the box, and both are load-bearing now. */
    expect([...touched]).toEqual(['opacity']);
    /* The floor itself is a contrast bound, not a taste — asserted against the
       painted mark in the next test rather than as a number here. */
    expect(pulseFloor(name)).toBeLessThan(1);
  });

  /*
   * ── CHANGED ASSERTION: the effective value, under the emulated feature ────
   *
   * What used to stand here searched the stylesheets for *some* rule whose
   * `selectorText` contained the dot's class, inside *some* reduced-motion
   * media block, with `animationName: 'none'` — and found one, while the rule
   * was inert. A media query adds no specificity, so a bare `.taskDot` (0,1,0)
   * lost outright to the pulse's `.taskDot[data-nc-status="running"]`
   * (0,2,0), and under emulated `reduce` the effective `animation-name` was
   * still `task-dot-pulse`. That test would have stayed green with the rule
   * deleted, or moved into any unrelated selector.
   *
   * So: emulate the feature, and read the value off the element the reader
   * actually sees. (The global sweep in `styles/base.css` was suppressing the
   * *motion* all along — `animation-duration: 0.01ms !important` — which is why
   * nothing looked wrong. It leaves the animation named and running once; this
   * rule is the exact one, and now it wins.)
   */
  it('really stops the pulse under an emulated prefers-reduced-motion', async () => {
    await browserPage.viewport(1200, 800);
    const dots = renderMarks();
    const running = dots.get('running')!;
    /* Premise: the pulse is on at all, so `none` below is a change and not the
       resting state of an element that never animated. (The name is the CSS
       Module's hashed one — `_task-dot-pulse_…` — so it is captured, not
       spelled.) */
    const pulsing = getComputedStyle(running).animationName;
    expect(pulsing).toContain('task-dot-pulse');

    await commands.emulateReducedMotion(true);
    expect(getComputedStyle(running).animationName).toBe('none');

    /* And it comes back — a rule that suppressed the pulse unconditionally
       would pass the line above and be a different bug. */
    await commands.emulateReducedMotion(false);
    expect(getComputedStyle(running).animationName).toBe(pulsing);
  });

  /*
   * ── The dimmest frame of the pulse is a colour, and it has a floor ───────
   *
   * `opacity` composites, so a fill at 0.35 over the card is not the fill: the
   * in-flight mark measured 1.43:1 in light for half of every breath, under the
   * 3:1 non-text floor the rest of this file is written to — and the one state
   * that is actually moving is the one it happened to. Neither contrast check
   * could see it: both measured the opaque token.
   *
   * Measured here the way the compositor would: the mark's own painted fill,
   * blended with the card at the keyframe's minimum opacity, against the card.
   * `tools/styles/check-contrast.mjs` makes the same measurement off the token
   * definitions and reads the same floor out of the stylesheet, so lowering it
   * reddens the style gate too.
   */
  it.each(['light', 'dark'] as const)(
    '%s: the in-flight mark clears the non-text floor at the dimmest frame of its pulse',
    async (theme) => {
      await browserPage.viewport(1200, 800);
      if (theme === 'dark') document.documentElement.dataset.theme = 'dark';
      const running = renderMarks().get('running')!;
      const card = paint(
        getComputedStyle(document.documentElement).getPropertyValue('--surface-card'),
      );
      /* Premise: the theme took, and the breath is a breath. */
      expect([theme, lightness(card) > 50]).toEqual([theme, theme === 'light']);
      const floor = pulseFloor(getComputedStyle(running).animationName);
      expect(floor).toBeGreaterThan(0);
      expect(floor).toBeLessThan(1);

      const fill = markColour(running);
      const dimmed = [0, 1, 2].map((i) => fill[i] * floor + card[i] * (1 - floor));
      expect([theme, contrast(dimmed, card) >= 3]).toEqual([theme, true]);
    },
  );
});

/*
 * ── The status, on hover ────────────────────────────────────────────────────
 *
 * Reported as "hover 没状态". The `title` attribute was never missing, and the
 * first hypothesis — that the reveal button's own `title` was winning, because
 * `elementFromPoint` at the dot returns the reveal control — is wrong twice
 * over: `elementFromPoint` returns the *dot*, and the earlier assertion only
 * reads the reveal control out of it because it calls `.closest('button')` on
 * the dot's own ancestors. Probed in the engine, the actual defect was that six
 * pixels from the dot's centre in any direction the topmost element is the bare
 * `<li>`, which carries no `title` at all — silence, not the wrong tooltip.
 *
 * So the assertion that matters is not "the attribute is set" (it was, and the
 * bug shipped) but **what the browser would resolve at the points a reader
 * actually puts the pointer**: the topmost element there, and the nearest
 * ancestor-or-self of it with a `title`. That is what a native tooltip does,
 * and it is measurable.
 */
const failedWithReason: ReportTaskRow = {
  blockId: 'b-bench', key: 'bench-harness', state: 'ready', declaration: null,
  status: 'failed', statusDetail: 'track 9a4c is not a git repository',
  kind: 'terminal', workerCardId: 'c-4', pendingReason: null,
};

/** What a native tooltip would show at this point, and where it came from. */
function tooltipAt(x: number, y: number): { source: Element | null; text: string | null } {
  const source = document.elementFromPoint(x, y)?.closest('[title]') ?? null;
  return { source, text: source?.getAttribute('title') ?? null };
}

describe('hovering the TASKS status', () => {
  it('answers with the status — reason and all — anywhere in the lane the row reserves for it', async () => {
    await browserPage.viewport(1200, 800);
    render(
      <div style={{ inlineSize: 1200, blockSize: 800 }}>
        <TrackPage
          track={track}
          cards={[]}
          tasks={[failedWithReason]}
          onOpenCard={vi.fn()}
          onOpenTask={vi.fn()}
          onRenameTrack={vi.fn()}
          onDeleteTrack={vi.fn()}
        />
      </div>,
    );
    const row = document.querySelector<HTMLElement>('[data-nc-task-inventory] li')!;
    const dot = row.querySelector<HTMLElement>('[data-nc-status]')!;
    const mark = dot.getBoundingClientRect();
    const phrase = 'failed — track 9a4c is not a git repository';

    /* Premise: the painted mark really is the small thing this is about. A dot
       that had quietly grown to fill the lane would make every point below a
       point *on* the mark, and the test would prove nothing about the lane. */
    expect(mark.width).toBeGreaterThan(0);
    expect(mark.width).toBeLessThanOrEqual(8);

    /*
     * Points spread across the lane, every one of them off the mark: left of
     * it, right of it, above, below, and the far corners. Each is checked to be
     * outside the mark's own rect before it is used, so none of these can pass
     * by accidentally landing on the 8px dot.
     */
    const probes: ReadonlyArray<readonly [number, number, string]> = [
      [0, 0, 'the mark itself'],
      [-7, 0, 'leading side of the lane'],
      [7, 0, 'trailing side, against the card edge'],
      [0, -7, 'above the mark'],
      [0, 7, 'below the mark'],
      [-7, -7, 'leading-top corner'],
      [7, 7, 'trailing-bottom corner'],
    ];
    const centre = { x: mark.left + mark.width / 2, y: mark.top + mark.height / 2 };
    for (const [dx, dy, where] of probes) {
      const x = centre.x + dx;
      const y = centre.y + dy;
      const offMark = x < mark.left || x > mark.right || y < mark.top || y > mark.bottom;
      /* Premise for every probe but the first: it is genuinely off the mark. */
      expect([where, offMark]).toEqual([where, dx !== 0 || dy !== 0]);

      const { source, text } = tooltipAt(x, y);
      expect([where, source === dot]).toEqual([where, true]);
      expect([where, text]).toEqual([where, phrase]);
    }
  });

  it('still gives the click to the reveal control across that whole lane', async () => {
    await browserPage.viewport(1200, 800);
    const onOpenCard = vi.fn();
    const onOpenTask = vi.fn();
    render(
      <div style={{ inlineSize: 1200, blockSize: 800 }}>
        <TrackPage
          track={track}
          cards={[]}
          tasks={[failedWithReason]}
          onOpenCard={onOpenCard}
          onOpenTask={onOpenTask}
          onRenameTrack={vi.fn()}
          onDeleteTrack={vi.fn()}
        />
      </div>,
    );
    const row = document.querySelector<HTMLElement>('[data-nc-task-inventory] li')!;
    const dot = row.querySelector<HTMLElement>('[data-nc-status]')!;
    const reveal = row.querySelector<HTMLElement>('button[title^="Show "]')!;
    const kind = row.querySelector<HTMLElement>('button[title^="Open the worker card"]')!;
    const mark = dot.getBoundingClientRect();
    const centre = { x: mark.left + mark.width / 2, y: mark.top + mark.height / 2 };

    /* The dot owns the hover; the reveal control still owns the action. This is
       the half that `pointer-events: none` would have bought at the cost of the
       hover — it is bought here by the dot being a DOM child instead. */
    for (const [dx, dy] of [[0, 0], [-7, 0], [7, 0], [0, 7]] as const) {
      const hit = document.elementFromPoint(centre.x + dx, centre.y + dy) as HTMLElement;
      expect(hit.closest('button')).toBe(reveal);
    }
    (document.elementFromPoint(centre.x + 7, centre.y) as HTMLElement).click();
    expect(onOpenTask).toHaveBeenCalledWith('b-bench');
    expect(onOpenCard).not.toHaveBeenCalled();

    /*
     * And the lane stops where the kind starts. The dot's target abuts the
     * kind's trailing edge; one pixel the other side of that line the kind is
     * still both the hover and the click, or widening the lane would have been
     * paid for out of the only control that opens the worker card.
     */
    const kindBox = kind.getBoundingClientRect();
    const insideKind = { x: kindBox.right - 1, y: kindBox.top + kindBox.height / 2 };
    expect(tooltipAt(insideKind.x, insideKind.y).source).toBe(kind);
    (document.elementFromPoint(insideKind.x, insideKind.y) as HTMLElement).click();
    expect(onOpenCard).toHaveBeenCalledWith('c-4');
  });

  /*
   * The word itself never depended on the pointer. `aria-label` on the dot
   * folds into the reveal button's accessible name, so a keyboard or
   * screen-reader user gets the same sentence by focusing the row — which is
   * the channel `title` cannot serve at all (no focus, no touch). This asserts
   * the two carriers agree, so a future edit cannot fix one and drop the other.
   */
  it('carries the same sentence in the accessible name, where the pointer is not', async () => {
    await browserPage.viewport(1200, 800);
    render(
      <div style={{ inlineSize: 1200, blockSize: 800 }}>
        <TrackPage
          track={track}
          cards={[]}
          tasks={[failedWithReason]}
          onOpenCard={vi.fn()}
          onOpenTask={vi.fn()}
          onRenameTrack={vi.fn()}
          onDeleteTrack={vi.fn()}
        />
      </div>,
    );
    const row = document.querySelector<HTMLElement>('[data-nc-task-inventory] li')!;
    const dot = row.querySelector<HTMLElement>('[data-nc-status]')!;
    const reveal = row.querySelector<HTMLElement>('button[title^="Show "]')!;

    expect(dot.getAttribute('aria-label'))
      .toBe('Status: failed — track 9a4c is not a git repository');
    /* The dot's name is part of the row's, not a second stop beside it. */
    expect(reveal.textContent).toBe('bench-harness');
    expect(dot.getAttribute('title')).toBe('failed — track 9a4c is not a git repository');
  });
});
