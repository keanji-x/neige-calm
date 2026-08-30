/*
 * The `/` menu's geometry and colour, against a real rendering engine.
 *
 * Everything this file asserts is a **third-party override**: the menu is
 * Astryx's popover, and `thread.module.css` reaches into it with CSS anchor
 * positioning and two of Astryx's own custom properties. Every one of those
 * overrides fails *silently* — an anchor name that stops resolving leaves the
 * menu content-width at the caret, a renamed variable leaves it the well's own
 * `--bg` with no shadow, and a portal to `document.body` would take out the
 * positioning and the colour together. In each case the menu still opens, still
 * lists the command, still runs it, and no test that reads the DOM notices.
 *
 * jsdom cannot see any of it: it computes no CSS, so `[popover]`, `anchor()`
 * and inherited custom properties are all inert there. This is the only tier
 * where these claims are falsifiable, which is why they are made here and
 * nowhere else.
 */
import { act, render } from '@testing-library/react';
import { page } from 'vitest/browser';
import { afterEach, describe, expect, it, vi } from 'vitest';

/*
 * The whole cascade, and **before the component**, both of which matter.
 *
 * `entry.css` opens with `@layer reset, vendor, tokens, base, astryx, ui,
 * features, overrides;` — a statement whose only job is to fix the order, and
 * which only fixes it for layers not already registered. `thread.module.css`
 * declares `@layer features` of its own, so importing the component first
 * registers `features` as the *first* layer in the document and hands every
 * one of this app's overrides a lower priority than Astryx's compiled StyleX.
 * Measured: in that order Send paints Astryx's own primary fill and the
 * override under test loses silently — the same class of failure this file
 * exists to catch, arriving through the test's own import order.
 *
 * A probe that imports a subset of the stylesheets, or the same set in another
 * order, is measuring a page that does not exist.
 */
import '../../../styles/entry.css';

import { ChatComposer } from './public.tsx';
import { useState } from '../../../ui/state/public.ts';

afterEach(() => { document.body.replaceChildren(); });

/**
 * The cascade order the document actually ended up with, read off the first
 * `@layer` rule in sheet order — which is the rule that *fixes* the order,
 * because `@layer` registration is first-come and later mentions cannot
 * reorder. Duplicated verbatim in `app/shell/drawer-seam.browser.test.tsx`
 * rather than shared: it is a probe of a file's own import order, so a copy
 * that travels with the file is the point.
 */
function registeredLayerOrder(): readonly string[] {
  for (const sheet of [...document.styleSheets]) {
    let rules: CSSRuleList;
    try { rules = sheet.cssRules; } catch { continue; }
    for (const rule of [...rules]) {
      if (rule instanceof CSSLayerStatementRule) return [...rule.nameList];
      if (rule instanceof CSSLayerBlockRule) return [rule.name];
    }
  }
  return [];
}

const PRODUCTION_LAYER_ORDER = [
  'reset', 'vendor', 'tokens', 'base', 'astryx', 'ui', 'features', 'overrides',
];

/** The composer as the drawer hands it over: a card that publishes the inset
 *  and radius `thread.module.css` reads off its host. */
function Card() {
  return (
    <div style={{ position: 'absolute', insetBlock: 20, insetInlineEnd: 24, inlineSize: 396 }}>
      <div
        style={{
          // The two customs `ui/drawer/drawer.module.css` sets on `.drawer`.
          ['--nc-card-inset' as string]: '8px',
          ['--nc-card-radius' as string]: '16px',
        }}
      >
        <ChatComposer onSend={vi.fn()} onNewConversation={vi.fn()} />
      </div>
    </div>
  );
}

/** Type `text` into the contenteditable with a real caret, the way
 *  `useTriggerMenu` reads it — it walks back from `window.getSelection()`, not
 *  from the value, so the caret has to exist. */
async function typeInto(field: HTMLElement, text: string) {
  field.textContent = text;
  const range = document.createRange();
  range.setStart(field.firstChild!, text.length);
  range.collapse(true);
  const selection = window.getSelection()!;
  selection.removeAllRanges();
  selection.addRange(range);
  await act(async () => {
    field.dispatchEvent(new InputEvent('input', { bubbles: true }));
    await Promise.resolve();
  });
  /* The menu is positioned by the engine after the popover is shown; one frame
     is what the measurements below need. */
  await act(async () => {
    await new Promise((resolve) => { requestAnimationFrame(() => { resolve(null); }); });
  });
}

const composer = () => document.querySelector<HTMLElement>('[data-nc-composer]')!;
const field = () => document.querySelector<HTMLElement>('[contenteditable="true"]')!;
const menu = () => document.querySelector<HTMLElement>('[role="listbox"]')!;
/** The positioned box is the popover, not the listbox inside it. */
const popover = () => menu().closest<HTMLElement>('[popover]') ?? menu();

async function openMenu() {
  await page.viewport(1400, 900);
  render(<Card />);
  await typeInto(field(), '/');
  expect(document.querySelector('[role="listbox"]')).not.toBeNull();
}

describe('the / command menu, as the engine lays it out', () => {
  /* The premise every measurement below rests on, made falsifiable. The prose
     at the top of this file explains why importing a subset of the stylesheets,
     or the same set in another order, measures a page that does not exist —
     this is that claim as a gate rather than as a warning. */
  it('registers the cascade in the production order', () => {
    expect(registeredLayerOrder()).toEqual(PRODUCTION_LAYER_ORDER);
  });

  /*
   * Equal width with the input well, which is the whole visual claim: the menu
   * is the well's own lid, not a floating list near the caret.
   *
   * `.composer`'s box is the well *plus* the card inset it is padded with, so
   * the menu's edges land one inset inside `.composer` on each side. Measured
   * off painted boxes, so it is red if `anchor-name` stops resolving, if the
   * `inline-size: auto` that beats the UA's `width: fit-content` is dropped, or
   * if the popover is portalled out of `.composer` and stops matching at all.
   */
  it('spans exactly the input well, one card inset inside the composer box', async () => {
    await openMenu();
    const box = composer().getBoundingClientRect();
    const lid = popover().getBoundingClientRect();
    const inset = 8;
    expect(lid.width).toBeGreaterThan(100);
    expect(Math.round(lid.left - box.left)).toBe(inset);
    expect(Math.round(box.right - lid.right)).toBe(inset);
    /* And it sits above the well rather than over it. */
    expect(Math.round(lid.bottom)).toBeLessThanOrEqual(Math.round(box.top + inset) + 1);
  });

  /*
   * The colour override, pinned on the *computed* value rather than on the
   * declaration.
   *
   * `--color-background-popover` and `--shadow-low` are Astryx's internal names,
   * set from `thread.module.css` on `.composer [popover]` and inherited down to
   * whichever element Astryx actually paints — here the popover's own child, not
   * the `[popover]` box, which is transparent. Renaming either name upstream is
   * a silent regression: the menu falls back to the surface Astryx picks with no
   * elevation, which is precisely the reading ("the lid is part of the well")
   * the design rejected, and nothing about the menu's behaviour changes.
   *
   * Both are compared against a probe carrying the *page's* token, so what is
   * pinned is that the hook still connects — not that some particular colour
   * string is spelled some particular way.
   */
  it('paints the menu on --paper with the float shadow, not on a fallback surface', async () => {
    await openMenu();
    /* The painted surface. `[popover]` itself is transparent; Astryx puts the
       fill and the elevation on the box inside it. */
    const surface = popover().firstElementChild as HTMLElement;
    const painted = getComputedStyle(surface);

    const probe = document.createElement('div');
    probe.style.backgroundColor = 'var(--paper)';
    probe.style.boxShadow = 'var(--shadow-float)';
    document.body.append(probe);
    const wanted = getComputedStyle(probe);

    expect(painted.backgroundColor).toBe(wanted.backgroundColor);
    expect(painted.boxShadow).toBe(wanted.boxShadow);

    /* And `--paper` is not what the well is filled with, so falling back to the
       composer's own surface cannot pass the line above. */
    const wellFill = getComputedStyle(composer()).getPropertyValue('--bg').trim();
    probe.style.backgroundColor = wellFill;
    expect(painted.backgroundColor).not.toBe(getComputedStyle(probe).backgroundColor);
    probe.remove();
  });

  /*
   * Send's fill, which is an override keyed on an **English string**.
   *
   * `thread.module.css` selects it as `.composer :is(button[aria-label='Send'])`
   * — Astryx's own hardcoded `label`. Localise the library, or let it reword the
   * button, and the selector stops matching: Send goes back to Astryx's accent
   * blue, in a card whose whole palette is paper and chip grey, and nothing
   * anywhere reports it. This is that report.
   *
   * The assertion is on the fill only. Which token it *is* (`--surface-chip`) is
   * a settled visual decision; what is fragile is whether the rule connects at
   * all, and comparing against a probe carrying the token is what distinguishes
   * "connected" from "fell back to the vendor default".
   */
  it('fills Send with the chip surface rather than Astryx\'s accent', async () => {
    await page.viewport(1400, 900);
    render(<Card />);
    /* With something to send: an unavailable Send is painted by a different
       rule, and the override under test is the live one. */
    await typeInto(field(), 'Ship it');
    const send = document.querySelector<HTMLElement>('button[aria-label="Send"]')!;

    const probe = document.createElement('div');
    probe.style.backgroundColor = 'var(--surface-chip)';
    document.body.append(probe);
    expect(getComputedStyle(send).backgroundColor).toBe(getComputedStyle(probe).backgroundColor);
    probe.remove();
  });
});

/*
 * ── Where the focus goes when Send disables itself, on the app's own wiring ──
 *
 * The unit tier renders `<ChatComposer onSend={…} />` with no `disabled` prop
 * and cannot answer this. Both router call sites pass `disabled={store.sending}`
 * and `send()` sets that flag synchronously, so the send and the disabling are
 * one commit: Astryx turns the message field into `contenteditable="false"` in
 * the same flush that takes the Send button out of the tab order. A `focus()`
 * issued before that flush is undone by it — Chromium hands the focus to
 * `<body>` when the element holding it stops being editable — and jsdom does
 * not model that at all, so this is the only tier where the fix is falsifiable.
 */
const flightsInProgress: Array<() => void> = [];

/** The composer as the router builds it: `disabled` goes true inside the very
 *  click that sends, and comes back only when the request lands. */
function Sending() {
  const [sending, setSending] = useState(false);
  return (
    <div style={{ inlineSize: 396 }}>
      <ChatComposer
        disabled={sending}
        onSend={() => {
          setSending(true);
          flightsInProgress.push(() => { setSending(false); });
        }}
      />
    </div>
  );
}

async function land() {
  const done = flightsInProgress.pop()!;
  await act(async () => { done(); await Promise.resolve(); });
}

describe('sending, with the disabled prop the router actually passes', () => {
  it('never leaves focus on <body> while the field is disabled, and returns it after', async () => {
    await page.viewport(1400, 900);
    render(<Sending />);
    await typeInto(field(), 'Ship it');
    const send = document.querySelector<HTMLElement>('button[aria-label="Send"]')!;
    send.focus();
    expect(document.activeElement).toBe(send);

    await act(async () => { send.click(); await Promise.resolve(); });

    /* Mid-flight: the field refuses focus (it is not editable) and Send has left
       the tab order, so the only wrong answer is `<body>`. Focus is parked on
       the composer's own box, which is inside the composer and is a real
       element the next Tab can continue from. */
    expect(document.querySelector('[contenteditable="true"]')).toBeNull();
    expect(document.activeElement).not.toBe(document.body);
    expect(composer().contains(document.activeElement)).toBe(true);

    await land();

    /* And the caret comes back to the field on its own, which is the whole
       promise: the standing request outlives the disabled window. */
    expect(document.activeElement).toBe(field());
  });

  /* Enter is the other way in, and it fails identically: focus starts in the
     field, and the field is what goes away. */
  it('returns the caret to the field after a send made with Enter', async () => {
    await page.viewport(1400, 900);
    render(<Sending />);
    await typeInto(field(), 'Ship it');
    field().focus();

    await act(async () => {
      field().dispatchEvent(new KeyboardEvent('keydown', {
        key: 'Enter', bubbles: true, composed: true, cancelable: true,
      }));
      await Promise.resolve();
    });
    expect(document.activeElement).not.toBe(document.body);

    await land();
    expect(document.activeElement).toBe(field());
  });
});

