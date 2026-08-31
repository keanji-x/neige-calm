/*
 * The exchange rail **as a finger gets it**, rendered.
 *
 * ── Why this is a file and not three more cases next door ─────────────────
 *
 * `pointer: coarse` is a media *feature*. Nothing inside a page can set one,
 * and the one lever a test had — `Emulation.setTouchEmulationEnabled` over CDP
 * — is a one-way door: turning it on gives `pointer: coarse`, turning it off
 * leaves the page at `pointer: none`, where **neither** branch of a
 * pointer-split stylesheet matches. Vitest's browser mode reuses one page for
 * every file in a project, so a single case that opened that door poisoned
 * every case after it, in every file. The coarse branch of
 * `thread.module.css` was therefore guarded for three rounds by reading its own
 * `cssText` out of `document.styleSheets` — a declaration-level read that
 * proved the rule was *written*, never that a coarse device *got* it.
 *
 * The way out is a browser context, not a page: `@vitest/browser-playwright`
 * takes `contextOptions`, and a project of its own gets a context of its own.
 * `vitest.config.ts` opens this one with `hasTouch` + `isMobile` and a phone
 * viewport, and the first case below asserts the three media queries that make
 * the rest of the file mean anything — including that `pointer: none` is
 * *false*, which is the shape of poisoning this arrangement exists to avoid.
 *
 * ── What that lets go ────────────────────────────────────────────────────
 *
 * `thread.browser.test.tsx` used to carry a case that read the coarse rule's
 * text, copied it onto a probe div, and asserted the block's ordinal position
 * in the document — because equal specificity means source order is the whole
 * of why the coarse block wins. Every part of that is subsumed here: a rendered
 * 28px row on a coarse page is only possible if the coarse block came last, so
 * moving it above `.rail` — the mutation that left the old file green at 35/35
 * — now shows up as a measured 12.
 *
 * The one claim a render cannot make is the universal negative, so it moves
 * here rather than being deleted: "no rule anywhere reads `--nc-dot-lift`
 * outside a fine-pointer condition" is a statement about rules that do not
 * exist yet, and the engine can only be asked about the ones that do.
 *
 * ── The fixture is a copy, deliberately ──────────────────────────────────
 *
 * `RailPane` and its helpers are duplicated from `thread.browser.test.tsx`
 * rather than shared. Importing them would mean importing a module whose
 * top-level `describe` blocks would then run in this project too, at a
 * viewport and a pointer type none of them was written for. A copy that
 * travels with the file is the same trade `registeredLayerOrder` takes there.
 */
import { act, render } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';

/* The whole cascade, and before the component — the same import-order
   requirement `thread.browser.test.tsx` opens with, for the same reason: a CSS
   Module imported first registers `@layer features` ahead of everything and
   hands this app's overrides the lowest priority in the document. */
import '../../../styles/entry.css';

import { ChatThread } from './public.tsx';
import type { Conversation, ConversationTurn } from '../../../../../core/domain/conversation.ts';
import drawerStyles from '../../../ui/drawer/drawer.module.css';

afterEach(() => { document.body.replaceChildren(); });

function railConversation(): Conversation {
  return {
    id: 'c1', waveId: 'w1', waveTitle: 'Ship the rewrite', title: null, kind: 'codex',
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

/** `--space-9` + `--space-11`, from `.drawer`'s `inset-block`: how much taller
 *  than its pane the host has to be for the card — and therefore the seam — to
 *  come out at exactly `paneHeight`. */
const DRAWER_BLOCK_INSETS = 20 + 28;

/**
 * The drawer as four boxes — host, card, pane, seam — carrying `ui/drawer`'s
 * own classes, so the rail is portalled into a real seam and clipped by a real
 * `overflow: hidden`. A hand-rolled pane renders no rail at all and every case
 * below would pass vacuously.
 *
 * The host is 380 wide because this is a phone: the context's viewport is 420,
 * and a fixture wider than the screen would put the seam off the side of it.
 */
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
      <div className={drawerStyles.drawer} data-nc-drawer="" style={{ animation: 'none' }}>
        <div
          className={drawerStyles.scroll}
          data-nc-drawer-scroll=""
          style={{ blockSize: paneHeight, flex: 'none' }}
        >
          <div className={drawerStyles.bodyInner} data-nc-rail-pane-inner="">
            <ChatThread conversation={railConversation()} turns={turns} />
          </div>
        </div>
      </div>
      <div className={drawerStyles.seam} data-nc-drawer-seam="" style={{ animation: 'none' }} />
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

/** The centre of a dot's button box, in client coordinates. */
function centre(index: number): number {
  const box = dots()[index].getBoundingClientRect();
  return box.top + box.height / 2;
}

/**
 * The conditions a grouping rule adds to everything inside it.
 *
 * `@layer` adds nothing — it orders the cascade and constrains nothing about
 * when a rule applies. `@media` contributes its own text, *conjoined* with
 * whatever it is already nested in rather than replacing it, so a fine block
 * inside an outer query still reads as fine.
 *
 * **Everything else is a failure written into the condition string.** This
 * walk backs a universal negative — "no rule anywhere reads `--nc-dot-lift`
 * outside a fine-pointer condition" — and a grouping rule the walk does not
 * recognise is precisely the case where it does not know what it is looking
 * at. Skipping one silently, which is what descending only into `@media` and
 * `@layer` did, let a `@supports` or `@container` block hold a lift consumer
 * with the whole file green. Naming the type in the string means the caller's
 * `toContain('pointer: fine')` fails on it and the failure message says which
 * construct arrived.
 */
function conditionsUnder(rule: CSSGroupingRule, outer: readonly string[]): string[] {
  if (rule instanceof CSSLayerBlockRule) return [...outer];
  if (rule instanceof CSSMediaRule) return [...outer, rule.conditionText];
  const text = 'conditionText' in rule ? String(rule.conditionText) : '';
  return [...outer, `<unhandled grouping rule ${rule.constructor.name}: ${text}>`];
}

/**
 * Every media condition under which some rule declares `needle`, walking layers
 * and nested conditions. `''` for a rule at the top level. Copied from
 * `thread.browser.test.tsx`, where it is about to have no callers left: the
 * claim it serves is the one this file's renders cannot make.
 *
 * Exported to the two cases below rather than inlined, because one of them
 * feeds it a stylesheet built to be the thing it must not miss.
 */
function mediaConditionsDeclaring(needle: string): string[] {
  const found: string[] = [];
  const walk = (rules: CSSRuleList, conditions: readonly string[]) => {
    for (const rule of [...rules]) {
      if (rule instanceof CSSGroupingRule) { walk(rule.cssRules, conditionsUnder(rule, conditions)); continue; }
      /* Nested style rules carry their declarations in child rules of their own,
         so the recursion is not only over grouping rules. */
      if (rule instanceof CSSStyleRule) {
        if (rule.style.cssText.includes(needle)) found.push(conditions.join(' and '));
        walk(rule.cssRules, conditions);
      }
    }
  };
  for (const sheet of [...document.styleSheets]) {
    let rules: CSSRuleList;
    try { rules = sheet.cssRules; } catch { continue; }
    walk(rules, []);
  }
  return found;
}

describe('the exchange rail on a coarse pointer, as the engine lays it out', () => {
  /*
   * ── The premise, asserted before anything is measured ─────────────────────
   *
   * Every other case in this file is a statement about a device, and a device
   * that is not the one claimed makes all of them vacuous — worse than absent,
   * because they would be green. So the three queries are read off the engine
   * first.
   *
   * `pointer: none` is here for a specific failure and not for symmetry. That
   * is the state CDP touch emulation leaves behind when it is switched off, and
   * in it *neither* branch of the rail's geometry matches: the file would then
   * measure whatever the base rule happened to say, and the coarse branch would
   * be untested again with no sign of it. Measured under this project's
   * context: coarse true, fine false, none false.
   *
   * The screen is asserted for the same reason `isMobile` is set with a
   * viewport: `isMobile` on a desktop-sized page is a handset claim with no
   * screen behind it, and the 320px cap two cases below is a block-axis fact
   * that only means anything against a real one. It is `screen`, not
   * `innerWidth`: the runner puts every suite in a 414 × 896 iframe whatever
   * the context says, so the inner box reads the same in *both* projects and
   * would be green on a desktop context. Measured — this project 420 × 900
   * with `maxTouchPoints` 1, the plain `browser` project 1280 × 720 with 0.
   *
   * **Both screen axes, and `isMobile` itself, because the name of this case
   * promises all three.** The height is the one the cap is read against, and
   * it was unasserted: a context dropped to 300 tall left this green. And
   * `isMobile` is a separate switch from `hasTouch` — deleting it leaves
   * `pointer: coarse`, `hover: none` and `maxTouchPoints` all exactly as they
   * are, so nothing above notices. What it does change is that Chromium stops
   * running the page in mobile emulation, and the two things that says so are
   * measured here: legacy `window.orientation` exists only under it (present
   * and 0 with `isMobile`, absent without), and `screen.orientation` reports
   * the orientation the 420 × 900 box actually has rather than the host's —
   * `portrait-primary` with it, and `landscape-primary` on the identical
   * viewport without it.
   */
  it('runs in a context that reports a coarse pointer and nothing else', () => {
    expect(matchMedia('(pointer: coarse)').matches).toBe(true);
    expect(matchMedia('(pointer: fine)').matches).toBe(false);
    expect(matchMedia('(pointer: none)').matches).toBe(false);
    expect(matchMedia('(any-pointer: coarse)').matches).toBe(true);
    expect(screen.width).toBe(420);
    expect(screen.height).toBe(900);
    expect(navigator.maxTouchPoints).toBeGreaterThan(0);
    expect('orientation' in window).toBe(true);
    expect(screen.orientation.type).toBe('portrait-primary');
  });

  /*
   * ── The geometry, measured instead of read ────────────────────────────────
   *
   * The coarse block redeclares three custom properties at the same specificity
   * as the base rule, so **source order is the entire reason a finger gets
   * them**. The case this replaces asserted that ordinal as a number, because
   * it had no other way to see it. A render does not need to: a 28px row can
   * only be laid out if the coarse block came last, and the mutation that
   * defeated the old assertion — the whole `@media (pointer: coarse)` block
   * moved above `.rail` — reads here as a 12px row and a 4px dot.
   *
   * The numbers, all measured under this context: the button box is 24 × 28,
   * consecutive centres are 28 apart, a resting dot's ink is 6px and the lit
   * one's is 8px. 28 is the same number as `--control-h` and the same number as
   * `--nc-rail-pitch-open` on the fine branch, and it is neither derived from
   * nor coupled to either — the stylesheet says so, and this case reads the
   * rendered box rather than any of the three.
   *
   * **The shoulders are asserted at zero, which is a claim about what a finger
   * does *not* pay for.** The fine branch spends two openings of blank at each
   * end of the column so that a spread cannot slide the rail out from under
   * the pointer. There is no spread here, so that blank would buy nothing and
   * is not spent; the rules that write it live inside `@media (pointer: fine)`.
   *
   * **A bare `0px` does not say that, and for one round it was what was
   * written here.** The shoulder is
   * `(--nc-rail-pitch-open − --nc-rail-pitch) × lead`, and this branch sets
   * `--nc-rail-pitch` to `--control-h` while leaving `--nc-rail-pitch-open` at
   * the 28px it always was — so the expression is `0px` gated or not, and
   * copying both shoulder rules verbatim out of the fine block left the file
   * green at 6/6. The measurement below is taken at a pitch where the two
   * numbers differ instead: `--nc-rail-pitch` is forced to the fine branch's
   * own 12px inline on the rail's element, where it outranks the coarse
   * block's declaration, and the rows are read back at 12 so the override is
   * known to have landed rather than assumed. The shoulder the fine rules
   * would write there is `(28 − 12) × 2 = 32px` at each end — measured, with
   * the two rules lifted out of the media block, exactly 32. Gated, 0.
   */
  it('lays out a 24 by 28 target at a flat 28px pitch, with no shoulders', async () => {
    render(<RailPane turns={railTurns(8)} />);
    await settle();

    const lit = litDot();
    expect(lit).toBeGreaterThanOrEqual(0);
    /* A dot that is not the lit one, so what is measured is the rest state. */
    const resting = lit === 1 ? 2 : 1;
    const box = dots()[resting].getBoundingClientRect();

    expect(box.height).toBe(28);
    expect(box.width).toBe(24);
    /* WCAG 2.5.8's number, on the branch that meets it by target size outright.
       Asserted as the criterion and then as the build, so cutting 28 back to
       the floor has to come past both. */
    expect(box.height).toBeGreaterThanOrEqual(24);
    expect(box.width).toBeGreaterThanOrEqual(24);

    /* Centre to centre, off the laid-out boxes, which is the distance a finger
       actually has to land inside. */
    expect(centre(2) - centre(1)).toBe(28);
    expect(centre(3) - centre(2)).toBe(28);

    /* The ink: 6 at rest, 8 on the exchange you are in. Both were the untested
       half of the coarse block — cut to `1px`, the old declaration-level case
       stayed green. */
    expect(dotInk(resting)).toBe(6);
    expect(dotInk(lit)).toBe(8);

    /* The shoulder rules are `:first-child` / `:last-child` on the track, so
       what they land on is the track's end *children* — which are the end dots
       only for as long as nothing else is rendered in there. Anchored, because
       otherwise a sibling slipped in ahead of the dots would give the reading
       below a third, unrelated reason to be zero. */
    expect(railTrack().firstElementChild).toBe(dots()[0]);
    expect(railTrack().lastElementChild).toBe(dots().at(-1));

    /* And now the fine branch's shoulders are absent at a pitch that would
       show them: 12px, inline on the rail's own element. */
    const rail = railTrack().parentElement!;
    rail.style.setProperty('--nc-rail-pitch', '12px');
    await frame();
    expect(dots()[resting].getBoundingClientRect().height).toBe(12);
    expect(getComputedStyle(dots()[0]).marginBlockStart).toBe('0px');
    expect(getComputedStyle(dots().at(-1)!).marginBlockEnd).toBe('0px');
    rail.style.removeProperty('--nc-rail-pitch');
  });

  /*
   * ── Nothing swells, and one thing is written that nothing reads ───────────
   *
   * A finger has no hover, so the spread is not offered here at all: what it
   * would produce is a swell — and now also a gap, which moves every dot below
   * it — latched under wherever a tap landed, with no second event to clear it.
   *
   * Two independent mechanisms say so, and this case holds both apart.
   *
   * **The component's guard**, which is the one that matters on a hybrid
   * device: a laptop with a touchscreen reports `pointer: fine`, so the media
   * query is not the guard there and `public.tsx`'s `pointerType === 'touch'`
   * check is the whole of it. Asserted here by dispatching a touch
   * `pointermove` over the track and reading zero `--nc-dot-lift` properties
   * off the dots.
   *
   * **The stylesheet's guard**, asserted as what it actually produces. A
   * `pointermove` carrying `pointerType: 'mouse'` on *this* page — a page with
   * no mouse — does publish a lift, and this case asserts that it does rather
   * than pretending otherwise: measured, dots 2 through 7 carry
   * `0.156 / 0.5 / 0.844 / 1 / 0.844 / 0.5`. **It is dead style.** Every rule
   * that reads the property sits inside `@media (pointer: fine)`, and the one
   * the shoulder consumers scale by — `(--nc-rail-pitch-open − --nc-rail-pitch)`
   * — is 28 − 28 = 0 on this branch, so there is nothing for a lift to
   * multiply even if a rule reached it. That is measured here, not argued: with
   * the full envelope published, the pitch is still 28 and the ink is still 6.
   *
   * It is left as a written fact rather than fixed. Fixing it means the
   * component asking `matchMedia('(pointer: fine)')` before it publishes —
   * a second copy of the stylesheet's condition, living in the file whose whole
   * point is that it must *not* trust the media query (that is the hybrid case
   * above). The cost of leaving it is roughly forty inline property writes per
   * frame on a device where a mouse `pointermove` over the rail is not a real
   * gesture; the cost of the fix is a duplicated condition that a hybrid device
   * would then be laid out by. Recorded so the next reader finds a decision
   * instead of an oversight.
   */
  it('publishes no lift for a finger, and lays nothing out from the one a mouse leaves', async () => {
    render(<RailPane turns={railTurns(8)} />);
    await settle();

    const move = (pointerType: string) => {
      railTrack().dispatchEvent(new PointerEvent('pointermove', {
        bubbles: true, pointerType, clientY: centre(5),
      }));
    };

    move('touch');
    await settle();
    await pause(150);
    for (const dot of dots()) expect(dot.style.getPropertyValue('--nc-dot-lift')).toBe('');
    expect(centre(6) - centre(5)).toBe(28);
    expect(dotInk(4)).toBe(6);

    /* The identical dispatch with a mouse, so the assertion above is about the
       component's guard rather than about the events not arriving. */
    move('mouse');
    await settle();
    await pause(150);
    const published = dots().filter((dot) => dot.style.getPropertyValue('--nc-dot-lift') !== '');
    expect(published.length).toBeGreaterThan(0);
    expect(dots()[5].style.getPropertyValue('--nc-dot-lift')).toBe('1');
    /* And it lays out nothing: same pitch, same ink, at full lift. */
    expect(centre(6) - centre(5)).toBe(28);
    expect(dotInk(5)).toBe(6);
    expect(dotInk(4)).toBe(6);

    railTrack().dispatchEvent(new PointerEvent('pointerleave', { pointerType: 'mouse' }));
    await settle();
    await pause(150);
  });

  /*
   * ── The universal negative, which is the part a render cannot do ──────────
   *
   * The case above measures that today's lift-reading rules produce nothing
   * here. It cannot say anything about a rule that does not exist yet — a new
   * declaration reading `--nc-dot-lift` outside a fine condition would change
   * some property this file never measures and stay green. So the sweep is over
   * the loaded stylesheet: every rule declaring the property, wherever it is,
   * has to sit under a `pointer: fine` condition.
   *
   * This is the one assertion that moved out of `thread.browser.test.tsx`'s
   * deleted case unchanged rather than being subsumed by a measurement.
   */
  it('keeps every rule that reads the published lift inside a fine condition', () => {
    const conditions = mediaConditionsDeclaring('--nc-dot-lift');
    expect(conditions.length).toBeGreaterThan(0);
    for (const condition of conditions) expect(condition).toContain('pointer: fine');
  });

  /*
   * ── And the sweep above is only worth its name if it cannot be walked past ─
   *
   * A universal negative asserted by a walk is only as strong as the walk's
   * coverage, and the first version of it descended into exactly two rule
   * types: `@media` and `@layer`. Every other grouping rule — `@supports`,
   * `@container`, `@scope`, `@starting-style` — was skipped whole, so a
   * consumer of `--nc-dot-lift` placed inside one was invisible to the case
   * above and the file stayed green with a coarse rail free to be magnified.
   *
   * So the single-violation fixture is here rather than in a reviewer's head:
   * one `@supports` block, declaring the property on the same pseudo-element
   * the real rules use, installed into this page's own stylesheet list. The
   * sweep has to come back with a condition for it, and that condition has to
   * be one the case above rejects. `@supports (color: red)` because it must
   * genuinely match — a block the engine drops is not a test of anything.
   */
  it('fails closed on a lift consumer inside a grouping rule it does not know', () => {
    const style = document.createElement('style');
    style.textContent = '@supports (color: red) { .railDot::before { --nc-dot-lift: 1; } }';
    document.head.append(style);
    try {
      const conditions = mediaConditionsDeclaring('--nc-dot-lift');
      const escaped = conditions.filter((condition) => !condition.includes('pointer: fine'));
      expect(escaped).toHaveLength(1);
      expect(escaped[0]).toContain('CSSSupportsRule');
      expect(escaped[0]).toContain('(color: red)');
    } finally {
      style.remove();
    }
  });

  /*
   * ── The floating prompt never appears ─────────────────────────────────────
   *
   * The preview is a hover affordance. A touchscreen's first pointer event on a
   * control is the press, so the only thing it could do here is paint a
   * description of where the reader has *already been taken*.
   *
   * Both halves are bound, and they are different mechanisms. The component's
   * `pointerType` guard is what stops the layer being created at all — the one
   * that matters on a hybrid laptop, where the media query does not fire. The
   * stylesheet's `display: none` is the backstop, and it is asserted the only
   * way a backstop can be: force the layer up through a mouse `pointerover` on
   * this coarse page, then read the *computed* display off the element. The
   * case this replaces read `.style.display` off the rule object, which is a
   * claim that the declaration was written.
   */
  it('never paints the prompt, by the guard and by the rule behind it', async () => {
    render(<RailPane turns={railTurns(8)} />);
    await settle();

    dots()[3].dispatchEvent(new PointerEvent('pointerover', {
      bubbles: true, pointerType: 'touch',
    }));
    await pause(600);
    expect(railPreview()).toBeNull();

    dots()[3].dispatchEvent(new PointerEvent('pointerover', {
      bubbles: true, pointerType: 'mouse',
    }));
    await pause(600);
    const preview = railPreview();
    expect(preview).not.toBeNull();
    expect(getComputedStyle(preview!).display).toBe('none');
    /* Not merely invisible: it occupies nothing, so it cannot be what a finger
       lands on. */
    expect(preview!.getBoundingClientRect().height).toBe(0);
  });

  /*
   * ── Scroll-within-scroll arrives at twelve, not at twenty-two ─────────────
   *
   * `--nc-rail-max` is 320px and the reason is on the property: a full-height
   * column of ink in the seam is scrollbar-shaped and gets dragged. What
   * nothing in this repository said until now is that **the cap is reached at
   * half the conversation length on a phone**, because the pitch is more than
   * twice as tall.
   *
   * Measured, both branches, same 320px cap, at rest:
   *
   *   coarse   11 exchanges → 308px, fits · 12 → 336px, overflows
   *   fine     21 exchanges → 320px, fits · 22 → 328px, overflows
   *
   * The fine numbers are not 320 ÷ 12: the fine branch also spends two
   * shoulders of 32px, so 256px of the cap is dots. Neither branch is wrong —
   * the track is a scrollport, `safe center` hands the overflow to the end, and
   * every dot stays reachable, which is asserted below rather than assumed. But
   * a reader on a phone meets the rail's second scroll at a *twelve*-exchange
   * conversation, which is an ordinary one, and that is worth having written
   * down somewhere that fails when it stops being true.
   *
   * The cap is read off the engine rather than repeated here, so a change to
   * `--nc-rail-max` moves this case's arithmetic with it and the thresholds are
   * what break.
   */
  it('overflows the 320px cap at twelve exchanges, and stays reachable past it', async () => {
    render(<RailPane turns={railTurns(11)} paneHeight={700} />);
    await settle();

    const cap = Number.parseFloat(getComputedStyle(railTrack()).maxBlockSize);
    expect(cap).toBe(320);
    expect(railTrack().clientHeight).toBe(cap);
    /* Eleven rows of 28 is 308, which is inside the cap: no second scroll.
       Read off the column rather than off `scrollHeight`, which cannot say it:
       `scrollHeight` is floored at `clientHeight`, so at eleven rows it is 320
       for the same reason it would be at one row, and asserting it against the
       cap pins nothing. The dots' own extent is the number that moves. */
    const column = dots().at(-1)!.getBoundingClientRect().bottom - dots()[0].getBoundingClientRect().top;
    expect(column).toBe(308);
    expect(railTrack().scrollHeight).toBe(railTrack().clientHeight);

    document.body.replaceChildren();
    render(<RailPane turns={railTurns(12)} paneHeight={700} />);
    await settle();

    /* Twelve is 336, and the twelfth row is the one that crosses. */
    expect(railTrack().scrollHeight).toBe(336);
    expect(railTrack().scrollHeight).toBeGreaterThan(railTrack().clientHeight);

    /* Accessibility holds, and this is the half that must not be lost with it:
       the overflow all goes to the *end*, so the first dot is inside the box at
       `scrollTop: 0` and the last is reachable by scrolling. A plain `center`
       instead of `safe center` fails the first of these — the early dots sit at
       a negative offset no gesture can reach. */
    const track = railTrack();
    track.scrollTop = 0;
    await settle();
    expect(dots()[0].getBoundingClientRect().top)
      .toBeGreaterThanOrEqual(track.getBoundingClientRect().top - 0.5);

    track.scrollTop = track.scrollHeight - track.clientHeight;
    await settle();
    expect(track.scrollTop).toBe(16);
    expect(dots()[11].getBoundingClientRect().bottom)
      .toBeLessThanOrEqual(track.getBoundingClientRect().bottom + 0.5);
  });
});
