/* The exchange rail as a finger gets it, in a browser context of its own (`vitest.config.ts`): `pointer: coarse` is a
   media feature nothing in a page can set. A tablet, not a phone: a phone width paints no seam at all. */
import { act, render } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';

/* The whole cascade before the component: a CSS Module imported first registers `@layer features` ahead of everything. */
import '../../../styles/entry.css';

import { ChatThread } from './public.tsx';
import type { Conversation, ConversationTurn } from '../../../../../core/domain/conversation.ts';
import drawerStyles from '../../../ui/drawer/drawer.module.css';

afterEach(() => { document.body.replaceChildren(); });

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
      <div className={drawerStyles.drawer} data-nc-drawer="" style={{ animation: 'none' }}>
        <div
          className={drawerStyles.scroll}
          data-nc-drawer-scroll=""
          style={{ blockSize: paneHeight, flex: 'none' }}
        >
          <div className={drawerStyles.bodyInner} data-nc-rail-pane-inner="">
            <ChatThread cards={{}} stalled={false} conversation={railConversation()} turns={turns} />
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

/** One place the cascade could hand `--nc-dot-lift` to this page. `media` is every enclosing `@media` condition, outermost first, kept separate: `not (pointer: fine)` and `(pointer: fine), (pointer: coarse)` do not survive being joined with ` and `. */
type LiftSite = { media: readonly string[]; where: string };

/** Whether `condition` names a fine pointer as a requirement. Only a plain conjunction counts: `not`, `,` and `or` each turn the feature into something the device may lack, so a fine-only disjunction is refused and reported rather than parsed. */
function narrowsToFine(condition: string): boolean {
  const text = condition.toLowerCase();
  if (/,|(?:^|[\s(])(?:not|or)(?:[\s(]|$)/.test(text)) return false;
  return /\((?:any-)?pointer:\s*fine\)/.test(text);
}

/** Whether `site` still has to be reported. Both halves must clear it: `matchMedia` on every enclosing condition closes negations and lists, and the text half closes device states that are not this one (a landscape rule is false on this portrait tablet and true the moment it is turned). `some`, not `every`: one fine condition anywhere in the chain is enough. */
function reachesHere(site: LiftSite): boolean {
  const matchesHere = site.media.every((condition) => matchMedia(condition).matches);
  return matchesHere || !site.media.some(narrowsToFine);
}

/** A backslash escape inside a custom-property name (`--nc-dot-l\69 ft`) names the same property, but Chromium keeps the escape verbatim in `cssText`. Nothing is decoded; an undecidable serialisation is reported, on the same footing as an unreadable sheet. */
const ESCAPED_NAME = /--[\w-]*\\/;

/** Every place a loaded stylesheet could hand `needle` to this page. Nothing is skipped: every container is descended into (`@keyframes` and `@property` are not grouping rules), `@import`ed and adopted sheets are walked, and a sheet whose `cssRules` throws is recorded as a site. Only `@media` narrows a site. */
function liftSites(needle: string): LiftSite[] {
  const found: LiftSite[] = [];
  const carries = (text: string) => text.includes(needle) || ESCAPED_NAME.test(text);

  function walk(rules: CSSRuleList, media: readonly string[], trail: readonly string[]) {
    for (const rule of [...rules]) {
      const label = (text: string) => [...trail, text];
      /* Style rules carry declarations *and*, since nesting, child rules. */
      if (rule instanceof CSSStyleRule) {
        const at = label(rule.selectorText);
        if (carries(rule.style.cssText)) found.push({ media: [...media], where: at.join(' › ') });
        walk(rule.cssRules, media, at);
        continue;
      }
      if (rule instanceof CSSMediaRule) {
        const text = rule.media.mediaText;
        walk(rule.cssRules, [...media, text], label(`@media ${text}`));
        continue;
      }
      if (rule instanceof CSSGroupingRule) {
        const text = 'conditionText' in rule ? String(rule.conditionText) : '';
        walk(rule.cssRules, media, label(`${rule.constructor.name} ${text}`.trimEnd()));
        continue;
      }
      if (rule instanceof CSSImportRule) {
        /* `@import url(…) (pointer: fine)` narrows the whole sheet and the condition lives on the rule: Chromium reports it on `rule.media.mediaText`, and the imported sheet's own `media.mediaText` is empty. */
        const own = rule.media.mediaText;
        const at = label(own === '' ? `@import ${rule.href}` : `@import ${rule.href} ${own}`);
        const inside = own === '' ? media : [...media, own];
        const inner = rule.styleSheet;
        /* A sheet that has not arrived is a sheet whose contents are unknown,
           which is the same standing as one that cannot be read. */
        if (inner === null) { found.push({ media: [...inside], where: `${at.join(' › ')} <not loaded>` }); continue; }
        walkSheet(inner, inside, at);
        continue;
      }
      if (carries(rule.cssText)) {
        found.push({ media: [...media], where: label(`${rule.constructor.name}`).join(' › ') });
      }
    }
  }

  function walkSheet(sheet: CSSStyleSheet, outer: readonly string[], trail: readonly string[]) {
    const media = sheet.media.mediaText === '' ? [...outer] : [...outer, sheet.media.mediaText];
    const at = [...trail, sheet.href ?? '<inline>'];
    let rules: CSSRuleList;
    try { rules = sheet.cssRules; } catch (error) {
      found.push({ media, where: `${at.join(' › ')} <unreadable: ${String(error)}>` });
      return;
    }
    walk(rules, media, at);
  }

  for (const sheet of [...document.styleSheets, ...document.adoptedStyleSheets]) walkSheet(sheet, [], []);
  return found;
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

  /* Source order is the entire reason a finger gets the coarse block. The shoulders are measured at a forced 12px pitch, where the fine rules would write 32px; at the coarse pitch the expression is `0px` gated or not. */
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
    expect(box.height).toBeGreaterThanOrEqual(24);
    expect(box.width).toBeGreaterThanOrEqual(24);

    expect(centre(2) - centre(1)).toBe(28);
    expect(centre(3) - centre(2)).toBe(28);

    expect(dotInk(resting)).toBe(6);
    expect(dotInk(lit)).toBe(8);

    /* The shoulder rules are `:first-child` / `:last-child` on the track, so the end children must be the end dots. */
    expect(railTrack().firstElementChild).toBe(dots()[0]);
    expect(railTrack().lastElementChild).toBe(dots().at(-1));

    const rail = railTrack().parentElement!;
    rail.style.setProperty('--nc-rail-pitch', '12px');
    await frame();
    expect(dots()[resting].getBoundingClientRect().height).toBe(12);
    expect(getComputedStyle(dots()[0]).marginBlockStart).toBe('0px');
    expect(getComputedStyle(dots().at(-1)!).marginBlockEnd).toBe('0px');
    rail.style.removeProperty('--nc-rail-pitch');
  });

  /* Two guards held apart: the component's `pointerType` check (the only one on a hybrid laptop, which reports `pointer: fine`) and the stylesheet's. A mouse `pointermove` on this page does publish a lift, and it is dead style: every consumer sits inside `@media (pointer: fine)`. Left as written rather than fixed, since the fix is a duplicated media condition. */
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

  /* A render cannot rule out a rule that does not exist yet, so the sweep is over every loaded stylesheet. The count is asserted first because the sweep's own failure mode is finding nothing. */
  it('keeps every rule that reads the published lift out of a finger’s reach', () => {
    const sites = liftSites('--nc-dot-lift');
    expect(sites.length).toBeGreaterThanOrEqual(3);
    expect(sites.filter(reachesHere).map(({ where }) => where)).toEqual([]);
  });

  /* Each fixture is one violation the sweep was once shown to miss; injected alone it must come back as exactly one reachable site naming the construct. `@supports (color: red)` and `@container (min-width: 1px)` are written to genuinely match. */
  it.each([
    ['@supports (color: red) { .railDot::before { --nc-dot-lift: 1; } }', 'CSSSupportsRule (color: red)'],
    ['@container (min-width: 1px) { .railDot::before { --nc-dot-lift: 1; } }', 'CSSContainerRule (min-width: 1px)'],
    ['@scope (body) { .railDot::before { --nc-dot-lift: 1; } }', 'CSSScopeRule'],
    ['@keyframes nc-lift-probe { to { translate: 0 var(--nc-dot-lift); } }', 'CSSKeyframesRule'],
    ['@property --nc-dot-lift { syntax: "<number>"; inherits: false; initial-value: 1; }', 'CSSPropertyRule'],
    ['@media not (pointer: fine) { .railDot::before { --nc-dot-lift: 1; } }', '@media not (pointer: fine)'],
    ['@media (pointer: fine), (pointer: coarse) { .railDot::before { --nc-dot-lift: 1; } }', '@media (pointer: fine), (pointer: coarse)'],
    ['@media not (pointer: fine) { @media (pointer: coarse) { .railDot::before { --nc-dot-lift: 1; } } }', '@media not (pointer: fine) › @media (pointer: coarse)'],
    /* False on this portrait context and true the moment it is rotated. */
    ['@media (pointer: coarse) and (orientation: landscape) { .railDot::before { --nc-dot-lift: 1; } }', '@media (pointer: coarse) and (orientation: landscape)'],
    /* `--nc-dot-l\69 ft` is `--nc-dot-lift`; Chromium serialises the escape back out unchanged. */
    ['.railDot::before { translate: 0 var(--nc-dot-l\\69 ft); }', '.railDot::before'],
  ])('reports the single lift consumer hidden in %s', (css, trail) => {
    const style = document.createElement('style');
    style.textContent = css;
    document.head.append(style);
    try {
      const reached = liftSites('--nc-dot-lift').filter(reachesHere);
      expect(reached).toHaveLength(1);
      expect(reached[0].where).toContain(trail);
    } finally {
      style.remove();
    }
  });

  /* Nesting is a conjunction: an inner condition that matches behind an outer one that does not is genuinely out of reach. */
  it('leaves a coarse block alone when the query around it does not match', () => {
    const style = document.createElement('style');
    style.textContent = '@media (any-pointer: fine) { @media (pointer: coarse) { .railDot::before { --nc-dot-lift: 1; } } }';
    document.head.append(style);
    try {
      expect(matchMedia('(any-pointer: fine)').matches).toBe(false);
      const sites = liftSites('--nc-dot-lift');
      expect(sites.filter(({ where }) => where.includes('any-pointer'))).toHaveLength(1);
      expect(sites.filter(reachesHere)).toEqual([]);
    } finally {
      style.remove();
    }
  });

  /* Both declarations are backslashes naming no property, so neither is a violation and the sweep must not throw. */
  it('reports nothing for backslashes that name no property, and does not throw', () => {
    const style = document.createElement('style');
    style.textContent = '.railDot::before { --x: "\\2d\\2d nc-dot-lift"; --y: \\110000; }';
    document.head.append(style);
    try {
      expect(liftSites('--nc-dot-lift').filter(reachesHere)).toEqual([]);
    } finally {
      style.remove();
    }
  });

  /* `document.styleSheets` does not list an `@import`ed sheet, and a cross-origin sheet's `cssRules` throws `SecurityError` while it still applies: the runner serves from `localhost`, so `127.0.0.1` is a different origin to the same server. */
  it('reports a lift consumer behind an @import', async () => {
    const url = URL.createObjectURL(new Blob(['.railDot::before { --nc-dot-lift: 1; }'], { type: 'text/css' }));
    const style = document.createElement('style');
    style.textContent = `@import url("${url}");`;
    document.head.append(style);
    try {
      const imported = style.sheet!.cssRules[0] as CSSImportRule;
      for (let attempt = 0; attempt < 200 && imported.styleSheet === null; attempt += 1) await pause(10);
      expect(imported.styleSheet).not.toBeNull();
      const reached = liftSites('--nc-dot-lift').filter(reachesHere);
      expect(reached).toHaveLength(1);
      expect(reached[0].where).toContain('@import');
      expect(reached[0].where).toContain('.railDot::before');
    } finally {
      style.remove();
      URL.revokeObjectURL(url);
    }
  });

  /* `@import url(…) (pointer: fine)` carries its condition on `CSSImportRule.media`; the imported sheet's own `media` is empty. */
  it('carries an @import’s own condition into the sheet it pulls in', async () => {
    const url = URL.createObjectURL(new Blob(['.railDot::before { --nc-dot-lift: 1; }'], { type: 'text/css' }));
    const style = document.createElement('style');
    style.textContent = `@import url("${url}") (pointer: fine);`;
    document.head.append(style);
    try {
      const imported = style.sheet!.cssRules[0] as CSSImportRule;
      for (let attempt = 0; attempt < 200 && imported.styleSheet === null; attempt += 1) await pause(10);
      expect(imported.styleSheet).not.toBeNull();
      /* Where the condition is, and where it is not. */
      expect(imported.media.mediaText).toBe('(pointer: fine)');
      expect(imported.styleSheet!.media.mediaText).toBe('');

      const site = liftSites('--nc-dot-lift').find(({ where }) => where.includes(url));
      expect(site).toBeDefined();
      expect(site!.where).toContain('.railDot::before');
      expect(site!.media).toContain('(pointer: fine)');
      expect(reachesHere(site!)).toBe(false);
    } finally {
      style.remove();
      URL.revokeObjectURL(url);
    }
  });

  it('reports a stylesheet whose rules it is not allowed to read', async () => {
    const link = document.createElement('link');
    link.rel = 'stylesheet';
    /* The runner's own stylesheet as CSS (`?direct` makes the dev server answer `text/css`), from the other name for the same host: cross-origin, so it applies but its rules are off limits. */
    link.href = `${location.origin.replace('localhost', '127.0.0.1')}/web/src/features/chat/thread/thread.module.css?direct`;
    document.head.append(link);
    try {
      await new Promise((resolve) => {
        link.addEventListener('load', resolve);
        link.addEventListener('error', resolve);
        setTimeout(resolve, 5_000);
      });
      const sheet = [...document.styleSheets].find((candidate) => candidate.ownerNode === link);
      expect(sheet).toBeDefined();
      expect(() => sheet!.cssRules).toThrow(/Cannot access rules/);

      const reached = liftSites('--nc-dot-lift').filter(reachesHere);
      expect(reached).toHaveLength(1);
      expect(reached[0].where).toContain('<unreadable:');
      expect(reached[0].where).toContain('127.0.0.1');
    } finally {
      link.remove();
    }
  });

  /* The component's `pointerType` guard stops the layer being created; the stylesheet's `display: none` is the backstop, asserted by forcing the layer up with a mouse `pointerover` and reading the computed display. */
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
    expect(preview!.getBoundingClientRect().height).toBe(0);
  });

  /* Under a finger the 320px cap is reached at twelve exchanges (11 → 308px, 12 → 336px); the fine branch reaches it at twenty-two. The cap is read off the engine. */
  it('overflows the 320px cap at twelve exchanges, and stays reachable past it', async () => {
    render(<RailPane turns={railTurns(11)} paneHeight={700} />);
    await settle();

    const cap = Number.parseFloat(getComputedStyle(railTrack()).maxBlockSize);
    expect(cap).toBe(320);
    expect(railTrack().clientHeight).toBe(cap);
    /* Read off the column rather than `scrollHeight`, which is floored at `clientHeight` and would be 320 at one row too. */
    const column = dots().at(-1)!.getBoundingClientRect().bottom - dots()[0].getBoundingClientRect().top;
    expect(column).toBe(308);
    expect(railTrack().scrollHeight).toBe(railTrack().clientHeight);

    document.body.replaceChildren();
    render(<RailPane turns={railTurns(12)} paneHeight={700} />);
    await settle();

    /* Twelve is 336, and the twelfth row is the one that crosses. */
    expect(railTrack().scrollHeight).toBe(336);
    expect(railTrack().scrollHeight).toBeGreaterThan(railTrack().clientHeight);

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
