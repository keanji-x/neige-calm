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
import { page, userEvent } from 'vitest/browser';
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

import { ChatComposer, ChatThread } from './public.tsx';
import type {
  Conversation, ConversationActivity, ConversationSystemEntry, ConversationTurn,
  OptimisticConversationTurn, TranscriptEntry,
} from '../../../../../core/domain/conversation.ts';
import { Drawer } from '../../../ui/drawer/public.tsx';
import drawerStyles from '../../../ui/drawer/drawer.module.css';
import { useState } from '../../../ui/state/public.ts';

afterEach(() => { document.body.replaceChildren(); });

/**
 * The cascade order the document actually ended up with, read off the first
 * top-level `@layer` rule in sheet order — which is the rule that *fixes* the
 * order, because registration is first-come and later mentions cannot reorder.
 * Duplicated verbatim in
 * `app/shell/drawer-seam.browser.test.tsx` rather than shared: it is a probe of a
 * file's own import order, so a copy that travels with the file is the point.
 *
 * **What it does not see, stated so nobody reads it as stronger than it is.**
 * It stops at the first top-level statement or block it finds and looks no
 * further, so three registrations are invisible to it: `@import ... layer(x)`
 * (a `CSSImportRule` carrying a `layerName`, not a layer rule), a layer opened
 * inside `@media`/`@supports`, and a stylesheet that registers layers *before*
 * the first sheet carrying a top-level `@layer`. Any of those could have fixed
 * a wrong order earlier than the statement this returns, and the probe would
 * report the statement and pass.
 *
 * That is a false-green in one direction only — it never reports a wrong order
 * for a right page — and the shape it exists to catch is the one that has
 * actually shipped twice: a CSS Module's own `@layer ui`/`@layer features`
 * block registering first because the component was imported before
 * `entry.css`. Those are plain top-level blocks in the sheets this file loads,
 * so the probe sees them. Hardening it into a full recursive walk would be
 * pinning cases this app's build does not produce; if `@import layer()` or a
 * conditional layer ever enters `styles/`, this needs to grow with it.
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

/** The same card with a turn in flight, which is the only state that renders
 *  Stop and therefore the only one that renders the queue control beside it. */
function RunningCard() {
  return (
    <div style={{ position: 'absolute', insetBlock: 20, insetInlineEnd: 24, inlineSize: 396 }}>
      <div
        style={{
          ['--nc-card-inset' as string]: '8px',
          ['--nc-card-radius' as string]: '16px',
        }}
      >
        <ChatComposer onSend={vi.fn()} onStop={vi.fn()} onNewConversation={vi.fn()} />
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

  /*
   * ── #1505's queue control, on the same terms as Send above ───────────────
   *
   * `.queueSend` is a CSS-Module rule on a button that lives inside Astryx's
   * `sendActions` slot, i.e. inside the vendor's compiled StyleX subtree. It
   * makes two claims in prose that only an engine can settle, and both fail the
   * way everything else in this file fails — silently, with the button still
   * there and still sending:
   *
   *   1. it is filled with `--surface-chip`, the same material the Send rule
   *      above gives Astryx's own button, so the two read as one thing; and
   *   2. it is 32px tall, which is Astryx's `.footer { min-height: 32px }`, so
   *      it sits on Stop's baseline instead of growing the row.
   *
   * Both are compared against the engine rather than against a literal: the
   * fill against a probe carrying the token (so "connected" is distinguishable
   * from "fell back"), and the height against **Stop's own measured box** (so
   * this is the claim actually being made — same baseline — rather than the
   * number 32 written down twice).
   */
  it('paints the queue control in Send\'s material and on Stop\'s baseline', async () => {
    await page.viewport(1400, 900);
    render(<RunningCard />);
    await typeInto(field(), 'while it works');

    const queue = document.querySelector<HTMLElement>('[data-nc-send-queued]')!;
    const stop = document.querySelector<HTMLElement>('button[aria-label="Stop"]')!;
    const painted = getComputedStyle(queue);

    const probe = document.createElement('div');
    probe.style.backgroundColor = 'var(--surface-chip)';
    document.body.append(probe);
    expect(painted.backgroundColor).toBe(getComputedStyle(probe).backgroundColor);
    /* And the well's own fill is not that colour, so a rule that failed to
       connect and inherited the composer's surface cannot pass the line above. */
    probe.style.backgroundColor = getComputedStyle(composer()).getPropertyValue('--bg').trim();
    expect(painted.backgroundColor).not.toBe(getComputedStyle(probe).backgroundColor);
    probe.remove();

    const queueBox = queue.getBoundingClientRect();
    const stopBox = stop.getBoundingClientRect();
    expect(queueBox.height).toBe(stopBox.height);
    /* Same row, not merely the same height: a wrapped footer would satisfy the
       line above and still be the layout this rule exists to prevent. */
    expect(Math.abs(queueBox.top - stopBox.top)).toBeLessThan(1);
    /* Left of Stop, which is what the `sendActions` slot means. */
    expect(queueBox.right).toBeLessThanOrEqual(stopBox.left + 1);
  });

  /*
   * The caption under a queued message: right-edged onto the turn it belongs
   * to, and legible.
   *
   * The alignment is a layout claim jsdom cannot make — `margin-inline-start:
   * auto` and `text-align: end` compute to nothing there — and it is the whole
   * of how the sentence reads as attached to the message above it rather than
   * as a centred seam like `.gap`.
   *
   * The colour is here because `check-contrast.mjs` does not read this
   * stylesheet (its pair table is `--warn*` / `--error*` and the plugin chips),
   * so the 4.5:1 this text needs has no gate of its own. The ratio is computed
   * from what the engine actually painted, so it covers the rule failing to
   * connect as well as the token being changed underneath it.
   */
  it('right-edges the queued caption onto its message and keeps it legible', async () => {
    await page.viewport(1400, 900);
    const queuedTurn: OptimisticConversationTurn = {
      id: 'echo-1', author: 'you', text: 'A message that is waiting its turn.',
      atMs: 4_000, serverHighWaterBefore: 0, queued: true,
    };
    render(<RailPane turns={[...railTurns(1), queuedTurn]} />);

    const note = document.querySelector<HTMLElement>('[data-nc-queued-note]')!;
    const said = document.querySelector<HTMLElement>('[data-nc-queued]')!;
    const painted = getComputedStyle(note);
    expect(painted.textAlign).toBe('end');
    /* Flush with the message's own trailing edge — the property `.gap`, the
       other small line in this transcript, deliberately does not have. */
    expect(Math.abs(note.getBoundingClientRect().right - said.getBoundingClientRect().right))
      .toBeLessThan(1);

    /* `contrast` is this file's own reader of *painted* values (it handles the
       `oklch()` Chromium serialises these tokens as); the ratio is therefore
       computed from what the engine drew, so a rule that failed to connect
       fails here as surely as a token lowered underneath it. */
    expect(contrast(painted.color, backgroundBehind(note))).toBeGreaterThanOrEqual(4.5);
  });
});

/** The nearest ancestor that actually paints, since the caption itself has no
 *  fill and a transparent ancestor is not what the text is read against. */
function backgroundBehind(element: HTMLElement): string {
  for (let node: HTMLElement | null = element; node !== null; node = node.parentElement) {
    const fill = getComputedStyle(node).backgroundColor;
    if (fill !== 'rgba(0, 0, 0, 0)' && fill !== 'transparent') return fill;
  }
  return getComputedStyle(document.body).backgroundColor;
}

/*
 * ── Where the focus goes when Send disables itself, on the app's own wiring ──
 *
 * The unit tier renders `<ChatComposer onSend={…} />` with no `disabled` prop
 * and cannot answer this. Both router call sites pass a `disabled` that goes
 * true inside the very click that sends — `disabled={store.sending}` on the
 * conversation path and `disabled={creating}` on the draft path
 * (`app/router/public.tsx`); two flags, one timing — and the handler behind
 * each sets it synchronously, so the send and the disabling are one commit:
 * Astryx turns the message field into `contenteditable="false"` in the same
 * flush that takes the Send button out of the tab order. A `focus()` issued
 * before that flush is undone by it — Chromium hands the focus to `<body>` when
 * the element holding it stops being editable — and jsdom does not model that
 * at all, so this is the only tier where the fix is falsifiable.
 */
const flightsInProgress: Array<() => void> = [];

/**
 * The composer as the router builds it: `disabled` goes true inside the very
 * click that sends, and comes back only when the request lands.
 *
 * `withStop` reproduces the other half of that wiring, and reproduces its
 * *timing*, which is the whole reason the race exists. The router's two flags
 * are not the same flag: `disabled={store.sending}` clears when the POST
 * returns, while `onStop={store.working || store.stopping ? … : undefined}`
 * stays on for as long as the agent is still producing. So Stop is a live
 * control **inside** the composer that outlives the disabled window — which is
 * exactly the moment the focus effect reruns. A fixture that dropped `onStop`
 * on landing would be measuring a control that no longer exists.
 */
function Sending({ withStop = false }: { withStop?: boolean }) {
  const [sending, setSending] = useState(false);
  const [working, setWorking] = useState(false);
  return (
    <div style={{ inlineSize: 396 }}>
      <ChatComposer
        disabled={sending}
        {...(withStop && working ? { onStop: () => undefined } : {})}
        onSend={() => {
          setSending(true);
          if (withStop) setWorking(true);
          flightsInProgress.push(() => { setSending(false); });
        }}
      />
      <button type="button" data-testid="elsewhere">Elsewhere</button>
    </div>
  );
}

async function land() {
  const done = flightsInProgress.pop()!;
  await act(async () => { done(); await Promise.resolve(); });
}

/** Send the draft with a **real** Enter, delivered by the browser rather than
 *  by `dispatchEvent`. The difference is not cosmetic: `:focus-visible` is
 *  decided from the engine's own record of the last interaction modality, and a
 *  synthetic `KeyboardEvent` does not write to it. The ring assertion below is
 *  only meaningful on this path. */
async function pressEnter() {
  await userEvent.keyboard('{Enter}');
  await act(async () => { await Promise.resolve(); });
}

const elsewhere = () => document.querySelector<HTMLElement>('[data-testid="elsewhere"]')!;

describe('sending, with the disabled prop the router actually passes', () => {
  it('parks focus on the composer, never on <body>, and returns it to the field after', async () => {
    await page.viewport(1400, 900);
    render(<Sending />);
    await typeInto(field(), 'Ship it');
    const send = document.querySelector<HTMLElement>('button[aria-label="Send"]')!;
    send.focus();
    expect(document.activeElement).toBe(send);

    await act(async () => { send.click(); await Promise.resolve(); });

    /*
     * Mid-flight the field refuses focus (it is not editable) and Send has left
     * the tab order. Asserted as an identity rather than as "somewhere inside
     * the composer": the composer's subtree includes Astryx's own hidden nodes
     * and any number of unrelated descendants, so a containment check passes for
     * landings that would be wrong. There is exactly one right answer here and
     * it is the perch.
     */
    expect(document.querySelector('[contenteditable="true"]')).toBeNull();
    expect(document.activeElement).toBe(composer());

    await land();

    /* And the caret comes back to the field on its own, which is the whole
       promise: the standing request outlives the disabled window. */
    expect(document.activeElement).toBe(field());
  });

  /* Enter is the other way in, and it fails identically: focus starts in the
     field, and the field is what goes away. Same identity assertions. */
  it('parks on the composer and returns the caret after a send made with Enter', async () => {
    await page.viewport(1400, 900);
    render(<Sending />);
    await typeInto(field(), 'Ship it');
    field().focus();

    await pressEnter();

    expect(document.activeElement).toBe(composer());

    await land();
    expect(document.activeElement).toBe(field());
  });

  /*
   * ── The perch is a place, so it has a name and it has no ring ─────────────
   *
   * Focus stops on this box for the whole length of a request, which makes it
   * two things at once that an anonymous `<div>` cannot be.
   *
   * *A name*, because a screen reader announces whatever focus lands on and the
   * box measured `role: null, aria-label: null` before this — the reader was
   * parked, for the length of the request, on nothing with a readable name.
   *
   * *No ring*, because `styles/base.css` paints every `:focus-visible` with a
   * 2px accent outline and Chromium grants `:focus-visible` to a programmatic
   * `focus()` when the last interaction was a key press. Enter-to-send is
   * exactly that, so the main sending path drew a full-width accent frame
   * around the bottom of the card and held it until the request landed —
   * keyboard users only, on their normal path.
   *
   * The `:focus-visible` match is asserted *first* and deliberately: without it
   * the outline reading would pass for the trivial reason that the pseudo-class
   * never engaged, and this test would be green on a page where the ring is
   * still waiting to happen.
   */
  it('parks on a named box that draws no focus ring, after a real keyboard send', async () => {
    await page.viewport(1400, 900);
    render(<Sending />);
    await typeInto(field(), 'Ship it');
    field().focus();
    await pressEnter();

    const perch = document.activeElement as HTMLElement;
    expect(perch).toBe(composer());
    expect(perch.getAttribute('role')).toBe('group');
    expect(perch.getAttribute('aria-label')).toBe('Message composer');
    /* And not the field's own name — two elements one nesting apart answering to
       the same name is ambiguous to anyone navigating by it. Read off the
       `label` the component passes Astryx rather than off the element, because
       mid-flight the field is not editable and is not in the DOM to be read. */
    expect(perch.getAttribute('aria-label')).not.toBe('Message');

    /* The premise: this really is a `:focus-visible` situation, so the ring
       rule really is being asked to apply. */
    expect(perch.matches(':focus-visible')).toBe(true);
    expect(getComputedStyle(perch).outlineStyle).toBe('none');

    await land();
  });

  /*
   * ── Focus the reader moved is focus the reader keeps ──────────────────────
   *
   * The restore is a standing request, so it outlives the disabled window — and
   * that is precisely what makes it dangerous: when `disabled` clears it fires,
   * and whatever the reader did in between is undone. Stop is the case that
   * matters, because a send is what puts Stop on screen and aiming at it is the
   * one thing anyone does mid-request; it lives *inside* the composer, so the
   * old give-up test ("focus has left `.composer`") did not see it.
   */
  it('leaves focus on Stop when the reader aimed at it during the flight', async () => {
    await page.viewport(1400, 900);
    render(<Sending withStop />);
    await typeInto(field(), 'Ship it');
    const send = document.querySelector<HTMLElement>('button[aria-label="Send"]')!;
    await act(async () => { send.click(); await Promise.resolve(); });

    const stop = document.querySelector<HTMLElement>('button[aria-label="Stop"]')!;
    /* The premise, stated: this control is inside the composer, which is why a
       containment-based give-up test cannot tell it from the perch. */
    expect(composer().contains(stop)).toBe(true);
    stop.focus();
    expect(document.activeElement).toBe(stop);

    await land();

    /* The second premise: Stop outlives the disabled window, so there is still
       something for focus to have been stolen *from*. */
    expect(document.querySelector('button[aria-label="Stop"]')).toBe(stop);
    expect(document.activeElement).toBe(stop);
    expect(document.activeElement).not.toBe(field());
  });

  /* And the same for somewhere else on the page entirely — the case the old
     containment test did cover, kept so the narrowing above cannot quietly
     lose it. */
  it('leaves focus where the reader put it outside the composer', async () => {
    await page.viewport(1400, 900);
    render(<Sending />);
    await typeInto(field(), 'Ship it');
    const send = document.querySelector<HTMLElement>('button[aria-label="Send"]')!;
    await act(async () => { send.click(); await Promise.resolve(); });

    elsewhere().focus();
    await land();

    expect(document.activeElement).toBe(elsewhere());
  });
});

/*
 * ── `focusOnMount`, in an engine that renders Astryx for real (#1211 S2) ───
 *
 * The landing a just-created track gets: the drawer opens on the planner
 * conversation and the caret has to be *in the message field*, because the
 * reader's first sentence is the track's intent.
 *
 * This is the same machinery the send-restore above uses, and it is here for
 * the same reason that one is: the effect finds the field with
 * `[contenteditable="true"], textarea`, and whether Astryx's editable carries
 * that attribute in the commit the composer mounts in is a question about
 * Astryx and about a real DOM. jsdom resolves the selector immediately and so
 * cannot tell "the caret reached the field" from "the caret is parked on the
 * composer's box with the request still standing" — and on that second
 * outcome the request never clears, because the effect's `[sendCount,
 * disabled]` deps do not move on this path. The reader would be left one Tab
 * away from the only control the page exists for, and the drawer no longer
 * pulls focus back to itself either (`ui/drawer`'s guard).
 *
 * The assertions are identities against the field, not "somewhere inside the
 * composer": the perch *is* inside the composer, and it is the failure.
 *
 * ── What this tier does *not* cover, and who does ──────────────────────────
 *
 * These cases render `ChatComposer` directly, so they prove the engine half
 * only: given the flag, the caret reaches Astryx's real editable. They say
 * nothing about the app reaching this component with the flag raised — drop
 * `focusOnMount` in `app/router/public.tsx`, or move the composer out of the
 * drawer's `footer`, and every case here stays green. The wiring half is
 * `app/router/track-untitled.test.tsx` ("opens the planner conversation with the
 * caret in the composer"), which drives the real router and the real create
 * and cannot see the engine question. **Neither tier alone proves the
 * landing**; that is why both exist and why each names the other.
 *
 * Also unlike the send-restore block above, these render no `disabled` prop —
 * "a configuration the app never builds", by that block's own words. It is
 * sound here for a reason that does not carry over: at mount the composer is
 * never in flight, so `disabled` is false on the commit these measure, and the
 * timing the restore fixture reproduces has not started. A case that needed the
 * flag to survive a disabled window would have to use `Sending`.
 */
describe('the caret a just-created track lands with', () => {
  it('lands in the message field itself, not on the composer’s perch', async () => {
    await page.viewport(1400, 900);
    render(
      <div style={{ inlineSize: 396 }}>
        <ChatComposer focusOnMount onSend={vi.fn()} onNewConversation={vi.fn()} />
      </div>,
    );

    expect(document.activeElement).toBe(field());
    expect(document.activeElement).not.toBe(composer());
  });

  /*
   * The documented edge of the interface, pinned rather than defended: the flag
   * is read once, at mount, and this component carries no `key` on the router's
   * path. So raising it again on a composer that is already standing does
   * nothing, and a caller that wants a second landing has to produce a second
   * mount. Production satisfies that by construction — the intent is stated by
   * a create, so the track, the drawer and this composer are all new — and the
   * `focusOnMount` note in `thread/public.tsx` says so; this is the assertion
   * behind that sentence.
   *
   * Red when the prop starts being watched over time, which would be the second
   * focus policy that note rejects — the giving-up rule below would then be
   * overridden by a rerender the reader did not cause.
   */
  it('ignores the flag being raised again on a composer that is already mounted', async () => {
    await page.viewport(1400, 900);
    const composerWith = (armed: boolean) => (
      <div style={{ inlineSize: 396 }}>
        <ChatComposer focusOnMount={armed} onSend={vi.fn()} />
        <button type="button" data-testid="elsewhere">Elsewhere</button>
      </div>
    );
    const { rerender } = render(composerWith(false));

    elsewhere().focus();
    expect(document.activeElement).toBe(elsewhere());

    await act(async () => { rerender(composerWith(true)); await Promise.resolve(); });

    expect(document.activeElement).toBe(elsewhere());
  });

  /*
   * And through the seam it actually arrives by: the composer is the drawer's
   * `footer`, and the drawer's own open effect runs *after* it. Wired this way
   * because the two focus policies meet here and nowhere else — a drawer that
   * still pulled focus to its container would leave the caret on the panel,
   * with this composer's request standing and nothing left to rerun it.
   */
  it('keeps the caret in the field when the drawer opens around it', async () => {
    await page.viewport(1400, 900);
    render(
      <Drawer open title="Planner chat" onClose={() => undefined} footer={<ChatComposer focusOnMount onSend={vi.fn()} />}>
        <p>the transcript</p>
      </Drawer>,
    );

    expect(document.activeElement).toBe(field());
  });
});

/*
 * ── The exchange rail, against a real scrollport ──────────────────────────
 *
 * Three of the rail's claims are invisible to the web-dom tier and to any
 * reading of the DOM, and all three fail silently.
 *
 * *What it costs the reading measure.* The rail takes a 10px gutter out of the
 * text column, and only while it is shown. Get that wrong in either direction
 * and the transcript keeps rendering and keeps scrolling: too little and the
 * dots sit on top of the first characters of every reply, which is the bug this
 * geometry replaced; too much and the measure the `.reply` note spent a table
 * arguing for quietly narrows. jsdom computes no CSS, so nothing there can tell.
 *
 * *Which dot is lit.* That is decided from painted boxes against a real
 * scrollport, and the three positions below are the ones the previous rule —
 * "the topmost opening marker currently intersecting" — got wrong. This is the
 * only tier where the automatic half of the rail exists at all.
 *
 * *That the dots can all be reached.* The rail has its own scrollport bounded by
 * the pane, and both halves of that (the bound, and the scrolling) are pure
 * layout.
 */

function railConversation(): Conversation {
  return {
    id: 'c1', trackId: 'w1', trackTitle: 'Ship the rewrite', title: null, kind: 'codex',
    state: 'idle', updatedAt: 0, turns: 0,
  };
}

/** A line of reply, repeated. Long replies are what make the rail's hard cases
 *  reachable: a marker only leaves the pane's top edge if the exchange under it
 *  is taller than the pane. */
const LINE = 'The reply runs on for a few lines so the pane has something to scroll. ';

/**
 * `count` exchanges. `longReplies` of them answer at length; the rest answer
 * with one word, which is both the most common shape of a real reply and the
 * geometry that makes the end of the scroll interesting — the last few markers
 * stay on screen at maximum scroll and can never be brought to the top.
 */
function railTurns(count: number, longReplies = count): ConversationTurn[] {
  return Array.from({ length: count }).flatMap((_unused, index) => [
    { id: `you-${index}`, author: 'you' as const, text: `Ask ${index}`, atMs: index * 2_000 },
    {
      id: `agent-${index}`,
      author: 'agent' as const,
      text: index < longReplies ? `Answer ${index}. ${LINE.repeat(12)}` : 'Short.',
      atMs: index * 2_000 + 1,
    },
  ]);
}

/**
 * A prompt longer than `RAIL_LABEL_MAX` and shorter than `RAIL_PREVIEW_MAX`,
 * which is the band in which the two renderings of it differ — the accessible
 * name is truncated and the floating preview is not. A fixture whose prompts
 * fit in both would make the preview's whole reason for existing invisible.
 */
const LONG_PROMPT = 'Rewrite the transcript so the reply keeps the report’s voice '
  + 'and the drawer keeps its measure, and say what it costs.';

/**
 * A prompt comfortably longer than `RAIL_PREVIEW_MAX`, for the other end of the
 * band: the cap was the one number in the preview that nothing read, and
 * raising it from 240 to 1000 left this file green.
 */
const OVERLONG_PROMPT = `${LONG_PROMPT} ${LINE.repeat(4)}`.replace(/\s+/g, ' ').trim();

/** `count` exchanges whose questions come from `promptAt`, differing only by
 *  ordinal — the case the rail's naming is hardest on and the one the preview
 *  is most useful in. */
function promptTurns(count = 8, promptAt: (index: number) => string = () => LONG_PROMPT):
ConversationTurn[] {
  return Array.from({ length: count }).flatMap((_unused, index) => [
    { id: `you-${index}`, author: 'you' as const, text: promptAt(index), atMs: index * 2_000 },
    { id: `agent-${index}`, author: 'agent' as const, text: 'Short.', atMs: index * 2_000 + 1 },
  ]);
}

/** The drawer's own block insets, which set how much taller than its pane the
 *  host has to be for the card to come out at exactly `paneHeight`.
 *  `--space-9` + `--space-11`, from `.drawer`'s `inset-block`. */
const DRAWER_BLOCK_INSETS = 20 + 28;

/**
 * The drawer, and it is a **four-box** fixture because the drawer is: a host,
 * the card, the pane inside it, and the **seam** beside it.
 *
 * ── Why the card and the seam are the drawer's own classes ────────────────
 *
 * The rail is not in the transcript any more. It is portalled into the strip of
 * page beside the card, which `ui/drawer` renders and marks
 * `data-nc-drawer-seam`, and which `features/chat` finds by walking up to
 * `[data-nc-drawer]`. So a fixture that hand-rolls a pane and stops there
 * produces no rail at all, and every case below would pass vacuously on a
 * component that had stopped rendering one.
 *
 * More than that: the two facts this move is *for* are both facts about the
 * drawer's own stylesheet. `.drawer` is `overflow: hidden`, which is what would
 * clip a rail that was still a descendant; `.seam` is the box whose geometry
 * puts the dots in the page's trailing pad. Reproducing either by hand would be
 * asserting against a copy. So the card and the seam here carry
 * `drawerStyles.drawer` and `drawerStyles.seam` — the real rules, from the real
 * module — and the mutation that removes the portal is genuinely caught by the
 * real clip rather than by a fixture that agreed to be clipped.
 *
 * ── What is a substitute, named so nobody reads more into it ──────────────
 *
 * *The animation is off.* Both boxes enter with `drawer-in` — 12px of
 * `translate` and a fade over `--motion-medium` — and every rect in this file
 * would be read mid-flight. It is disabled inline here and asserted on its own
 * terms in the case that covers entering and leaving.
 *
 * *The pane takes a fixed `blockSize`* where the real `.scroll` is
 * `flex: 1; min-block-size: 0` inside the drawer's column. The host is then
 * `paneHeight + DRAWER_BLOCK_INSETS` tall so that the card — and therefore the
 * seam, which shares its `inset-block` — comes out at exactly `paneHeight`.
 * That equality is what lets a case say "the track is bounded by the drawer"
 * and compare against a number it already has.
 *
 * *The host stands in for `.main`*: `position: relative` so the two absolute
 * boxes resolve against it, `container-type: inline-size` and a `--conversation-span`
 * so `.drawer`'s own `inline-size` and the preview's `cqi` cap behave as they
 * do in the app. 396 is the established regression-fixture width. One case
 * deliberately narrows it to 240 so an overlong preview becomes tall enough
 * to exercise the edge clamp; that value is a test condition, not a claim
 * about the product's new 352px desktop floor.
 *
 * ── The 36px of block-start clearance, which is unchanged and still load-
 *    bearing ────────────────────────────────────────────────────────────────
 *
 * `.bodyInner` is the real class too, so the pane's inner column keeps its
 * `--nc-card-inset + --control-h` = 36px of top padding — the clearance for the
 * floating close. One thing in `features/chat` is still decided by it: at
 * `scrollTop 0` the first exchange's marker is 36px *below* the pane's top
 * edge, so the active rule's loop breaks on its first iteration and its "if
 * none of them, the first" fallback is the answer. With no block padding the
 * loop assigned on that first iteration instead and the fallback was
 * unreachable — replacing it with the *last* marker left this file green.
 *
 * The second thing it used to decide is gone with the sticky rail: a track
 * sized by the pane's height used to hang off the bottom of the drawer, because
 * sticky does not lift an element to its inset. The track is in the seam now
 * and bounded by the seam, so that case no longer exists.
 */
function RailPane({ turns, paneHeight = 400, conversationSpan = 396 }: {
  turns: readonly TranscriptEntry[];
  paneHeight?: number;
  conversationSpan?: number;
}) {
  return (
    <div
      data-nc-rail-host=""
      style={{
        position: 'relative',
        containerType: 'inline-size',
        blockSize: paneHeight + DRAWER_BLOCK_INSETS,
        inlineSize: 900,
        ['--conversation-span' as string]: `${conversationSpan}px`,
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

/** The drawer's own top clearance, which the fixture above reproduces from the
 *  same tokens the drawer uses. Read back off the engine rather than written
 *  down twice. */
function paneInsetTop(): number {
  const inner = document.querySelector<HTMLElement>('[data-nc-rail-pane-inner]')!;
  return Number.parseFloat(getComputedStyle(inner).paddingBlockStart);
}

const pane = () => document.querySelector<HTMLElement>('[data-nc-drawer-scroll]')!;
const railTrack = () => document.querySelector<HTMLElement>('[data-nc-rail-track]')!;
const dots = () => [...document.querySelectorAll<HTMLElement>('button[aria-label^="Jump to "]')];
const markers = () => [...document.querySelectorAll<HTMLElement>('[data-nc-exchange]')];
const currentDot = () => dots().findIndex((dot) => dot.getAttribute('aria-current') === 'true');
const replies = () => [...document.querySelectorAll<HTMLElement>('[data-nc-turn="agent"]')];

async function frame() {
  await act(async () => {
    await new Promise((resolve) => { requestAnimationFrame(() => { resolve(null); }); });
  });
}

describe('the structured system disclosure in a real engine', () => {
  it('has a full control-height target and toggles from the keyboard', async () => {
    const system: ConversationSystemEntry = {
      id: 'system-1', author: 'system', label: 'Report edited',
      text: 'The report changed in the kernel.', atMs: 0,
    };
    render(<RailPane turns={[system]} />);
    await frame();

    const details = document.querySelector<HTMLDetailsElement>('[data-nc-turn="system"]')!;
    const summary = details.querySelector<HTMLElement>('summary')!;
    const disclosure = summary.querySelector<HTMLElement>('[aria-hidden="true"]')!;
    expect(summary.getBoundingClientRect().height).toBeGreaterThanOrEqual(24);
    expect(getComputedStyle(summary).justifyContent).toBe('flex-start');
    expect(disclosure.textContent).toBe('›');

    summary.focus();
    await userEvent.keyboard('{Enter}');
    expect(details.open).toBe(true);
    expect(getComputedStyle(disclosure).transform).not.toBe('none');
  });
});

/** Two frames: one for the scroll handler's own rAF, one for the render it
 *  schedules. */
async function settle() {
  await frame();
  await frame();
}

/** Put the pane where a reader would have put it, by the same `scrollTop` write
 *  the rail's own press makes — so what is measured is the rule's reaction to a
 *  position, not the press that produced it. */
async function scrollPaneTo(top: number) {
  pane().scrollTop = top;
  await settle();
}

const railPreview = () => document.querySelector<HTMLElement>('[data-nc-rail-preview]');

/** The painted diameter of a dot's ink — the `::before`, not the button, which
 *  is the pitch tall whatever the envelope is doing. */
function dotInk(index: number): number {
  return Number.parseFloat(getComputedStyle(dots()[index], '::before').width);
}

/** Wait out a real interval inside `act`, so a `setTimeout` that lands in
 *  component state is flushed rather than warned about. */
async function pause(ms: number) {
  await act(async () => { await new Promise((resolve) => { setTimeout(resolve, ms); }); });
}

/**
 * Put the pointer at a client-Y inside the rail and let the envelope settle.
 *
 * Dispatched at the track, which is where the component listens, and with an
 * explicit `clientY` because the whole rule under test is a function of exactly
 * that number. The wait is longer than the `--motion-instant` transition on the
 * dot's size (0.06s), because a computed size read mid-transition is a point on
 * the interpolation rather than the answer.
 */
async function pointRailAt(clientY: number) {
  railTrack().dispatchEvent(new PointerEvent('pointermove', {
    bubbles: true, pointerType: 'mouse', clientY,
  }));
  await settle();
  await pause(150);
}

/**
 * One rule written against one of `element`'s own classes: the rule itself, the
 * media conditions it sits under, the pseudo-element it targets, and **where it
 * sits in document order**.
 */
type RailRule = Readonly<{
  rule: CSSStyleRule;
  at: number;
  conditions: readonly string[];
  pseudo: string;
}>;

/**
 * Every style rule in the document written against one of `element`'s own
 * classes, in document order.
 *
 * The classes are read **off the live element** rather than named here, which
 * is what stops this drifting onto a hashed CSS-module name the component no
 * longer carries — the test cannot pass against a rule that is not on the frame
 * the rail is actually in. It is a string comparison rather than
 * `element.matches(rule.selectorText)` because `architecture/no-class-dom-query`
 * fails a dynamic selector closed, and rightly: a *runtime* locator built from
 * a stylesheet is exactly the shape that rule exists to stop. Nothing here is
 * located by it; the element came from a data hook.
 *
 * **`at` is the reason this returns more than the rules.**
 * `@media (prefers-reduced-motion: reduce)` writes the *same* declaration at
 * the *same* specificity as the `@media (pointer: fine)` block above it, so the
 * only thing that makes it win is that it comes later — which is invisible to a
 * declaration-level read. `at` counts every style rule the walk passes, in
 * sheet order, so "later than the fine block" is a number two assertions can
 * compare.
 *
 * `@media (pointer: coarse)` used to be read the same way here and no longer
 * is: `thread.coarse.browser.test.tsx` runs in a browser context that reports
 * a coarse pointer and measures the rendered row, which can only come out at
 * 28px if that block won on source order. Reduced motion has no such context
 * yet — emulating the feature on this shared page poisons every file after it
 * — so this stays the ordinal read for that one condition.
 *
 * A selector's trailing pseudo-element is split off rather than ignored, so
 * `.railDot` and `.railDot::before` are told apart, and `.railDot:hover::before`
 * — a different rule about a different state — matches neither.
 */
function ruleLedgerFor(element: Element): RailRule[] {
  const own = new Set([...element.classList].map((name) => `.${name}`));
  const found: RailRule[] = [];
  let at = 0;
  const walk = (rules: CSSRuleList, conditions: readonly string[]) => {
    for (const rule of [...rules]) {
      if (rule instanceof CSSMediaRule) {
        walk(rule.cssRules, [...conditions, rule.conditionText]);
        continue;
      }
      if (rule instanceof CSSLayerBlockRule) { walk(rule.cssRules, conditions); continue; }
      if (!(rule instanceof CSSStyleRule)) continue;
      at += 1;
      const pseudo = /::[a-z-]+$/.exec(rule.selectorText)?.[0] ?? '';
      const base = rule.selectorText.slice(0, rule.selectorText.length - pseudo.length);
      if (own.has(base)) found.push({ rule, at, conditions, pseudo });
    }
  };
  for (const sheet of [...document.styleSheets]) {
    let rules: CSSRuleList;
    try { rules = sheet.cssRules; } catch { continue; }
    walk(rules, []);
  }
  return found;
}

/** The subset of a ledger sitting under a condition naming `needle`. */
function under(ledger: readonly RailRule[], needle: string): RailRule[] {
  return ledger.filter((entry) => entry.conditions.some((text) => text.includes(needle)));
}

describe('the exchange rail, as the engine lays it out', () => {
  /*
   * ── The gutter is gone, and this is the case that has to keep it gone ─────
   *
   * Three rounds argued about how wide to cut a channel through the transcript
   * for the dots: zero (with a hit box that overhung the paragraph), 14px, then
   * 10px. The rail is in the drawer's seam now and the answer is **none**, so
   * what this case asserts has turned inside out — but it has to stay bound at
   * *both* ends, because the shape it replaces was a fake gate twice over. The
   * old relative form ("the column shifts by the rail's own measured width")
   * held at any width whatsoever: executed at 0 and at 60px, both green.
   *
   * So there are two independent numbers here and neither is relative to the
   * other:
   *
   *   1. **The transcript does not move**, asserted as an identity between the
   *      railed and un-railed renders rather than as "shifts by zero" — the
   *      latter is what a 60px gutter would also satisfy if the comparison were
   *      against the rail's own box.
   *   2. **The rail is 24px wide**, against the literal and against the seam's
   *      own box, so neither the seam nor the track can move alone.
   *
   * Regression-checked in both directions, which is what the round-two failure
   * cost: giving `.threadFrame` a `grid-template-columns: 10px minmax(0, 1fr)`
   * back fails (1); a `.rail` forced to 0 or to 60px fails (2).
   */
  it('spends nothing on the transcript, and lives in the drawer’s seam', async () => {
    await page.viewport(1400, 900);
    render(<RailPane turns={railTurns(2)} />);
    await frame();
    expect(dots()).toHaveLength(0);
    const bare = replies()[0].getBoundingClientRect().left;
    /* The transcript starts on the card's own inset and nothing else. */
    expect(Math.round(bare)).toBe(Math.round(pane().getBoundingClientRect().left + 8));

    document.body.replaceChildren();
    render(<RailPane turns={railTurns(8)} />);
    await frame();
    expect(dots()).toHaveLength(8);

    /* (1) Every paragraph is exactly where it was with no rail at all. */
    for (const paragraph of replies()) {
      expect(Math.round(paragraph.getBoundingClientRect().left)).toBe(Math.round(bare));
    }
    /* And the frame it lives in declares no column track to give away. */
    const frameBox = document.querySelector<HTMLElement>('[data-nc-thread]')!.parentElement!;
    expect(getComputedStyle(frameBox).display).not.toBe('grid');

    /* (2) The rail is the seam wide — 24px, `--space-10`, the trailing page pad
       the card's own `inset-inline-end` leaves empty at every viewport. */
    const seam = document.querySelector<HTMLElement>('[data-nc-drawer-seam]')!;
    const rail = railTrack().getBoundingClientRect();
    expect(Math.round(seam.getBoundingClientRect().width)).toBe(24);
    expect(Math.round(rail.width)).toBe(24);

    /* The pitch, on the same terms — declared on `.rail` and the dot's own box
       is what has to follow it.

       **12, and it used to be 24, and this is not a WCAG claim any more.** The
       24px row was chosen so the button enclosed a 24px square and 2.5.8 was
       met by target size. That is over: the resting row is 12 tall, the aim is
       bought by the spread instead, and the assertion that guards the aim is
       `opens at least 24px of aim…` below — which is where the number 24 now
       lives and which measures the *rendered distance between targets* rather
       than a declaration. Deleting this one without putting that one in its
       place would be dropping a criterion, so they went in together.

       What is asserted here is the density itself, in both directions: the
       declared pitch, and the dot's own box following it. Executed at 24 —
       i.e. the change reverted — this line is red, which is what makes it the
       density's assertion and not a restatement of the stylesheet. */
    const pitch = Number.parseFloat(
      getComputedStyle(railTrack()).getPropertyValue('--nc-rail-pitch'),
    );
    expect(pitch).toBe(12);
    const dot = dots()[0].getBoundingClientRect();
    expect(Math.round(dot.height)).toBe(pitch);
    /* The width is the seam's and did not move: only the block axis got
       denser, so a dot is a 24 × 12 target at rest. */
    expect(Math.round(dot.width)).toBe(24);
    /* And the dots really are a pitch apart, centre to centre — the row height
       is only the aim if the rows are flush, and a stray margin or gap in the
       column would make the two numbers different things. */
    const centres = dots().map((each) => {
      const box = each.getBoundingClientRect();
      return box.top + box.height / 2;
    });
    for (let index = 1; index < centres.length; index += 1) {
      expect(centres[index] - centres[index - 1]).toBeCloseTo(12, 1);
    }

    /* And nothing operable is anywhere near the words: the whole rail is
       outside the card, past its trailing edge. */
    const card = document.querySelector<HTMLElement>('[data-nc-drawer]')!;
    for (const each of dots()) {
      expect(each.getBoundingClientRect().left)
        .toBeGreaterThanOrEqual(card.getBoundingClientRect().right);
    }
  });

  /*
   * ── The clip that makes the portal necessary ──────────────────────────────
   *
   * `.drawer` is `overflow: hidden` — the clip the card's corner radius cuts
   * against — so a rail rendered as a descendant of the card and reaching into
   * the seam is simply not painted. This is the case that says so, and it is
   * the one the "put it back inside" mutation fails: a copy of the rail
   * parented into the pane at the seam's own inline position is measurably
   * clipped away, while the real one is not.
   *
   * Asserted through `checkVisibility()` plus the box, because a clipped
   * element still reports a rect — the clip is a paint fact, not a layout one,
   * so a rect comparison alone would pass on the mutation.
   */
  it('paints the rail outside the card’s clip', async () => {
    await page.viewport(1400, 900);
    render(<RailPane turns={railTurns(8)} />);
    await frame();
    const card = document.querySelector<HTMLElement>('[data-nc-drawer]')!;
    expect(getComputedStyle(card).overflow).toBe('hidden');
    /* The real rail is not inside the box that clips. */
    expect(card.contains(railTrack())).toBe(false);
    expect(railTrack().checkVisibility()).toBe(true);

    /* A probe put where the rail would be if it were still a descendant: same
       inline position, inside the card. The card's own clip takes it. */
    const probe = document.createElement('div');
    probe.style.cssText = 'position:absolute;inset-block-start:0;'
      + 'inset-inline-start:100%;inline-size:24px;block-size:24px;background:red';
    document.querySelector<HTMLElement>('[data-nc-rail-pane-inner]')!.append(probe);
    const clipped = probe.getBoundingClientRect();
    expect(clipped.left).toBeGreaterThanOrEqual(card.getBoundingClientRect().right);
    probe.remove();
  });

  /*
   * The rail stays where the reader can reach it. The failure this guards has
   * not changed — dots pinned to the top of a transcript that scrolls away from
   * them, "a jump list you have to scroll back up to use" — but the mechanism
   * that prevents it has, completely.
   *
   * It used to be `position: sticky` inside the scrolling pane, so the rail
   * flowed 36px down at `scrollTop 0`, rose to its `--space-4` inset once the
   * scroll reached it, and stayed pinned after — three positions, the middle
   * one measured, and a per-frame custom property published so the track could
   * be sized against wherever it had got to.
   *
   * In the seam it does not move **at all**. The seam is absolutely positioned
   * against the same box the card is, so scrolling the transcript changes
   * nothing about the rail's box. That is a stronger claim than the old one and
   * a simpler one, and it is asserted as an exact identity across the full
   * range of the scroll rather than as a sequence of offsets: any residual
   * coupling to the pane's scroll shows up as a moved rect.
   */
  it('holds the rail still while the transcript scrolls under it', async () => {
    await page.viewport(1400, 900);
    render(<RailPane turns={railTurns(8)} />);
    await frame();
    await scrollPaneTo(0);
    const at0 = railTrack().getBoundingClientRect();
    /* `.rail` starts at the seam's own top, which is the card's — not at the
       pane's 36px of close-clearance, which is where the sticky rail used to
       flow. It is read one level up from the track because the track is no
       longer the whole seam: `--nc-rail-max` caps it and `.rail` centres what
       is left, so the *track's* top is the seam's top only on a drawer short
       enough that the cap does not bite. `.rail` is still the full strip, and
       is the box this claim was always about. */
    const seam = document.querySelector<HTMLElement>('[data-nc-drawer-seam]')!;
    const rail = railTrack().parentElement!.getBoundingClientRect();
    expect(rail.top).toBeCloseTo(seam.getBoundingClientRect().top, 0);
    expect(rail.top).not.toBeCloseTo(pane().getBoundingClientRect().top + paneInsetTop(), 0);

    await scrollPaneTo(400);
    /* The transcript really did move under it. */
    expect(replies()[0].getBoundingClientRect().top)
      .toBeLessThan(pane().getBoundingClientRect().top);
    expect(railTrack().getBoundingClientRect().top).toBeCloseTo(at0.top, 0);

    await scrollPaneTo(900);
    expect(railTrack().getBoundingClientRect().top).toBeCloseTo(at0.top, 0);
    /* And nothing about the rail is sticky any more — the property is the
       mechanism the paragraph above deletes, so its absence is asserted. */
    expect(getComputedStyle(railTrack().parentElement!).position).toBe('relative');
  });

  /*
   * ── The track is bounded twice, and the tighter bound is a fixed length ───
   *
   * `--nc-rail-room` is gone: the track is `block-size: 100%` of a seam whose
   * height is `inset-block: var(--space-9) var(--space-11)`, so the dots are
   * bounded by the card's own top and bottom edges as a CSS fact. The fixture
   * makes the card exactly `paneHeight` tall (see `RailPane`), so that bound is
   * a number this case already has.
   *
   * **And then by `--nc-rail-max`, which on any drawer over 320px tall is the
   * one that bites.** The reason is a shape, not a length: a thin column of ink
   * running the full height of the page's trailing pad *is* a scrollbar as far
   * as a reader's hands are concerned, and gets grabbed. So the assertion is
   * the pair — a track that stops at 320 inside a 400px seam, with clear seam
   * above and below it — because the cap without the centring is just a short
   * scrollbar and the centring without the cap is a full-height one.
   *
   * Both halves fail on their own mutation: `max-block-size` deleted gives 400
   * and no clearance, and `.rail`'s `justify-content: center` deleted leaves
   * 320 pinned to the seam's top edge, where the two clearances stop being
   * equal.
   *
   * Forty exchanges is well past what fits, which is what makes the bound
   * observable rather than moot.
   */
  it('caps the track at a fixed length and centres it in the seam', async () => {
    await page.viewport(1400, 900);
    render(<RailPane turns={railTurns(40, 0)} paneHeight={400} />);
    await frame();
    const seam = document.querySelector<HTMLElement>('[data-nc-drawer-seam]')!.getBoundingClientRect();
    const track = railTrack().getBoundingClientRect();
    expect(Math.round(seam.height)).toBe(400);
    /* The cap, against the literal and against the declaration, so neither can
       move without the other. */
    const cap = Number.parseFloat(
      getComputedStyle(railTrack()).getPropertyValue('--nc-rail-max'),
    );
    expect(cap).toBe(320);
    expect(Math.round(track.height)).toBe(320);
    expect(track.bottom).toBeLessThanOrEqual(seam.bottom + 0.5);
    /* Centred: the same page above it as below it, and both of them real. */
    expect(track.top - seam.top).toBeCloseTo(seam.bottom - track.bottom, 0);
    expect(track.top - seam.top).toBeGreaterThan(1);
    /* It really is overflowing — the bound is doing something. */
    expect(railTrack().scrollHeight).toBeGreaterThan(railTrack().clientHeight);
    /* And no custom property is being published for it any more. */
    const frameBox = document.querySelector<HTMLElement>('[data-nc-thread]')!.parentElement!;
    expect(frameBox.style.getPropertyValue('--nc-rail-room')).toBe('');
    expect(frameBox.style.getPropertyValue('--nc-rail-reach')).toBe('');
  });

  /*
   * ── The column sits in the middle of the seam while it fits ───────────────
   *
   * The dots used to hang from the seam's top edge, so the shortest rail this
   * feature ever shows — it appears at five exchanges — was a stub of ink in
   * the top corner of a 900px window. `.railTrack` is now the seam's full
   * height with `justify-content: safe center`, and this is the half of that
   * pair that says "centre".
   *
   * Asserted as an identity between two *block centres* — the dots' own, from
   * the first one's top to the last one's bottom, and the seam's — rather than
   * as a pair of offsets, so no arithmetic here has to repeat the pitch. The
   * two "not on the edge" lines after it are what make the identity mean
   * something: a track that had lost its centring entirely would put the
   * column at the top, where the midpoint comparison is the only thing that
   * catches it, and this states the same fact in the direction a reader
   * complained about.
   */
  it('centres a column that fits in the seam’s own block centre', async () => {
    await page.viewport(1400, 900);
    render(<RailPane turns={railTurns(8)} paneHeight={400} />);
    await frame();
    const track = railTrack();
    /* The premise: eight dots at a 24px pitch inside a 400px seam. A column
       that overflowed would be `safe`-aligned to the start and this case would
       be asserting the fallback instead of the rule. */
    expect(dots()).toHaveLength(8);
    expect(track.scrollHeight).toBeLessThanOrEqual(track.clientHeight + 1);

    const seam = document.querySelector<HTMLElement>('[data-nc-drawer-seam]')!
      .getBoundingClientRect();
    const first = dots()[0].getBoundingClientRect();
    const last = dots()[7].getBoundingClientRect();
    expect((first.top + last.bottom) / 2).toBeCloseTo((seam.top + seam.bottom) / 2, 0);
    /* And it is genuinely off both edges, which is the reader-visible half. */
    expect(first.top).toBeGreaterThan(seam.top + 1);
    expect(last.bottom).toBeLessThan(seam.bottom - 1);
  });

  /*
   * ── …and gives the centring up entirely rather than hide a dot ────────────
   *
   * This is the case that exists because of how the centring is spelled. A
   * plain `justify-content: center` on a scrollport puts half of any overflow
   * *before* the scroll origin: the leading dots end up at a negative
   * `scrollTop`, which is not a position any gesture can reach, and they are
   * gone for good. That is the same "the dots you cannot get to" defect the
   * scrollport itself was introduced to fix, arriving through a different
   * property.
   *
   * `safe center` degrades to `start` the moment the content stops fitting, so
   * all of the overflow goes to the end where the scrollport can reach it. Both
   * ends are asserted, and the *first* one is the assertion that fails on the
   * naive keyword — it was run with plain `center` to confirm exactly that.
   */
  it('keeps the first and the last dot reachable when the column overflows', async () => {
    await page.viewport(1400, 900);
    render(<RailPane turns={railTurns(40, 0)} paneHeight={400} />);
    await frame();
    const track = railTrack();
    expect(dots()).toHaveLength(40);
    /* The premise: there is more rail than seam, so alignment has overflow to
       distribute and the choice of keyword is observable. */
    expect(track.scrollHeight).toBeGreaterThan(track.clientHeight + 1);

    track.scrollTop = 0;
    await settle();
    const atTop = track.getBoundingClientRect();
    const first = dots()[0].getBoundingClientRect();
    expect(first.top).toBeGreaterThanOrEqual(atTop.top - 0.5);
    expect(first.bottom).toBeLessThanOrEqual(atTop.bottom + 0.5);

    track.scrollTop = track.scrollHeight;
    await settle();
    const atBottom = track.getBoundingClientRect();
    const last = dots()[39].getBoundingClientRect();
    expect(last.bottom).toBeLessThanOrEqual(atBottom.bottom + 0.5);
    expect(last.top).toBeGreaterThanOrEqual(atBottom.top - 0.5);
  });

  /*
   * ── The focus ring, measured against the box that actually clips it ───────
   *
   * This case used to compare the ring's leading stroke against `pane()`, and
   * it passed on a ring that was being cut in half — the pane is two boxes out
   * and clips nothing here. The box that clips is `.railTrack`: it is
   * `overflow-y: auto`, so `overflow-x` computes to `auto` too, and the dot is
   * `inline-size: 100%` of it, so a ring at the global `outline-offset: 2px`
   * reaches 4px past each inline edge with nothing but clip on the other side.
   * The stylesheet's `.railDot:focus-visible` note has the whole argument and
   * the fix.
   *
   * The reach is read off the *focused* dot rather than assumed: the ring only
   * exists under `:focus-visible`, which Chromium grants to a programmatic
   * `focus()`, and the old case read `outlineOffset` from a dot that had no
   * ring at all. Both inline edges, because a ring clipped on one side is the
   * same defect as a ring clipped on both.
   */
  it('keeps the focus ring inside the box that clips it', async () => {
    await page.viewport(1400, 900);
    render(<RailPane turns={railTurns(8)} />);
    await frame();
    dots()[0].focus();
    await frame();

    const ring = getComputedStyle(dots()[0]);
    expect(ring.outlineStyle).toBe('solid');
    const width = Number.parseFloat(ring.outlineWidth);
    expect(width).toBeGreaterThan(0);

    /* How far the outline's outer edge falls beyond the border box: the
       offset pushes the outline edge outward and the stroke is painted
       outward from there, so a negative offset brings the whole ring in. */
    const reach = Number.parseFloat(ring.outlineOffset) + width;
    const dot = dots()[0].getBoundingClientRect();
    const track = railTrack().getBoundingClientRect();
    expect(dot.left - reach).toBeGreaterThanOrEqual(track.left - 0.01);
    expect(dot.right + reach).toBeLessThanOrEqual(track.right + 0.01);
  });

  /*
   * ── 1.4.11 on the one control whose whole affordance is a 4px circle ──────
   *
   * There is no text on this button and no border: the dot *is* the control, so
   * 3:1 against the surface behind it is the requirement, and it is measured off
   * the rendered pseudo-element rather than off the token names. The
   * repository's `check-contrast.mjs` says of itself that it is "deliberately a
   * small semantic-pair check, not a CSS/DOM contrast audit" protecting "only
   * the explicitly listed text/fill recipes", and that "actual inherited
   * foregrounds and ancestor backgrounds require a browser render audit" — a
   * pseudo-element's fill over whichever ancestor is behind it is that audit.
   *
   * **The surface changed, so this case had to be re-derived rather than
   * re-run.** In the transcript's gutter the dot sat on `--surface-card`; in
   * the seam it sits on the page, `--bg`. That is a real shift in both themes
   * and it moves every ratio: on today's tokens `--text-3` goes 7.00 → 6.34
   * light and 6.47 → 8.10 dark, `--text-4` 2.11 → 1.91 and 1.82 → 2.28. So the
   * *background* this reads is the seam's, not the pane's, and reading the
   * pane's would be measuring against a surface the dot is no longer on.
   *
   * The shipped ink is neither token: `oklch(58% 0.01 250)` light at 3.81:1 and
   * `oklch(56% 0.012 245)` dark at 3.49:1 — the quietest grey that still clears
   * 3:1 with room, which is what "greyer than the text ladder" has to mean if
   * it is to mean anything checkable. The upper bound is asserted as well as
   * the lower, and that is the half that keeps this honest: without it,
   * reverting to `--text-3` passes.
   *
   * Bound against `--text-4` explicitly, because that is the ink someone
   * reaching for "quieter still" reaches for and it is 1.91:1 here — the
   * assertion has to fail on it rather than merely not mention it.
   */
  it('paints the resting dot between 3:1 and the text ladder, against the page', async () => {
    await page.viewport(1400, 900);
    render(<RailPane turns={railTurns(8)} />);
    await frame();
    const seam = document.querySelector<HTMLElement>('[data-nc-drawer-seam]')!;
    const ink = getComputedStyle(dots()[0], '::before').backgroundColor;
    /* **What is behind the dot is the page, and this reads it rather than
       naming it.** The seam paints nothing of its own — asserted, because a
       seam that grew a background would silently change every ratio below —
       and `--bg` is painted on `body` (`styles/base.css`), so `body` is the
       surface. The whole point of this case is that this is *not*
       `--surface-card`, which is what the pane paints and what the dot used to
       sit on. */
    const surface = getComputedStyle(document.body).backgroundColor;
    expect(getComputedStyle(seam).backgroundColor).toBe('rgba(0, 0, 0, 0)');
    expect(surface).not.toBe(getComputedStyle(pane()).backgroundColor);

    const ratio = contrast(ink, surface);
    expect(ratio).toBeGreaterThanOrEqual(3);
    /* Clear of the boundary rather than sitting on it: the 3:1 line is between
       `L62%` and `L64%` in this theme, and 3.2 is where a token nudge would
       cross it. */
    expect(ratio).toBeGreaterThan(3.2);
    /* And quieter than the ink ladder — this is a scale to glance at, not
       body text down the edge of the window. `--text-3` is 6.34:1 here. */
    expect(ratio).toBeLessThan(5);

    /* The two tokens that were considered, measured against the same surface so
       the numbers in the stylesheet's table are this file's numbers too. */
    const token = (name: string) =>
      getComputedStyle(document.documentElement).getPropertyValue(name).trim();
    expect(contrast(token('--text-4'), surface)).toBeLessThan(3);
    expect(contrast(token('--text-3'), surface)).toBeGreaterThan(5);

    /* Hover and the lit dot are on the same new surface and were re-checked
       rather than assumed to survive the move. */
    expect(contrast(token('--text-2'), surface)).toBeGreaterThanOrEqual(3);
    expect(contrast(token('--accent'), surface)).toBeGreaterThanOrEqual(3);
  });

  /*
   * ── The envelope, which is what a 4px dot is aimed with ───────────────────
   *
   * The dot under the pointer grows, **and so do its neighbours, by less**, and
   * the second half is the whole claim: a rail that magnified only the dot
   * being pointed at would tell you which one you are on at the moment you no
   * longer need to be told, and would leave a 4px target 4px wide until the
   * pointer was already inside it. The falloff is what widens the thing you are
   * aiming at *before* you arrive.
   *
   * So this asserts the shape and not merely the peak: out from the pointer,
   * strictly decreasing, ending flat at the resting size exactly
   * `RAIL_SPREAD_SPAN` dots away. Executed with the falloff removed — only the
   * nearest dot lifted — and the first neighbour reads the resting 4px, which
   * is this case red.
   *
   * **And the shape is a smoothstep, which "strictly decreasing" does not say.**
   * A straight ramp is strictly decreasing too, ends at exactly zero four dots
   * out, and satisfied every assertion here — executed, green, against a
   * comment on `RAIL_SPREAD_SPAN` that spends a paragraph rejecting it. The two
   * curves are told apart by where they sit near each end of the span, because
   * smoothstep leaves and arrives *flat*: at one dot out it is above the line
   * (0.844 against 0.750) and at three it is below (0.156 against 0.250). At a
   * 4px rest and an 8px peak that is 7.375 against 7.00, and 4.625 against
   * 5.00 — the two bounds below, which a straight ramp fails at both ends.
   *
   * At *two* dots out the two curves agree exactly (0.5, i.e. 6px), which is
   * why the discriminating pair is 1 and 3 rather than any adjacent pair. The
   * span changed from three to four with the pitch, so these numbers are not
   * the ones the previous round asserted; they were recomputed, not adjusted.
   */
  it('swells the dots around the pointer and settles back to rest', async () => {
    await page.viewport(1400, 900);
    render(<RailPane turns={railTurns(12, 2)} />);
    await frame();
    await scrollPaneTo(0);
    /* Park the real pointer off the rail before reading a resting size: the
       engine fires boundary events when the element under a stationary cursor
       changes, so a mouse left on the gutter by an earlier case would have this
       measuring a dot that is already lifted. */
    await userEvent.hover(pane());
    await pause(150);
    const rest = dotInk(11);
    expect(rest).toBe(4);

    const aimed = dots()[5].getBoundingClientRect();
    await pointRailAt(aimed.top + aimed.height / 2);

    /* The peak is the stylesheet's `--nc-rail-dot-peak`, and every dot converges
       on it whatever it rests at — read off the token rather than written down
       twice. */
    const peak = Number.parseFloat(
      getComputedStyle(railTrack()).getPropertyValue('--nc-rail-dot-peak'),
    );
    expect(peak).toBe(8);
    expect(dotInk(5)).toBeCloseTo(peak, 1);

    /* The arc: each step out is smaller than the last, and the fourth is back
       at rest because the span is four dots. */
    expect(dotInk(6)).toBeGreaterThan(dotInk(7));
    expect(dotInk(7)).toBeGreaterThan(dotInk(8));
    expect(dotInk(8)).toBeGreaterThan(dotInk(9));
    expect(dotInk(6)).toBeGreaterThan(rest + 1);
    expect(dotInk(7)).toBeGreaterThan(rest + 0.5);
    expect(dotInk(9)).toBeCloseTo(rest, 1);
    /* The smoothstep, as the two numbers a straight ramp cannot produce — one
       from each end of the span, because the two curves cross in the middle. */
    expect(dotInk(6)).toBeGreaterThan(7.2);
    expect(dotInk(8)).toBeLessThan(4.8);
    /* Symmetric: the pointer is between dots, not on a ramp. */
    expect(dotInk(4)).toBeCloseTo(dotInk(6), 1);

    /* And leaving lands back at exactly nothing — no residue, and no inline
       property left behind for the next pointer to inherit. A stuck swell is
       the one visible failure this mechanism can have. */
    railTrack().dispatchEvent(new PointerEvent('pointerleave', { pointerType: 'mouse' }));
    await settle();
    await pause(150);
    /* Dot 0 is the lit one and rests at `--nc-rail-dot-current`, not at
       `--nc-rail-dot` — the envelope is what it returns *from*, not a size it
       imposes. */
    expect(dotInk(0)).toBeCloseTo(6, 1);
    for (let index = 1; index < 12; index += 1) expect(dotInk(index)).toBeCloseTo(rest, 1);
    for (const dot of dots()) expect(dot.style.getPropertyValue('--nc-dot-lift')).toBe('');
  });

  /*
   * ── The 24px, measured where it now happens ───────────────────────────────
   *
   * **This is the assertion the whole density decision rests on, and it exists
   * because the one it replaces stopped being true.** One round ago the fine
   * row was 24px tall and `spends nothing on the transcript…` asserted a 24×24
   * button: WCAG 2.5.8 met by target size, at rest, permanently. The row is 12
   * now. That assertion was not deleted — deleting an assertion that a design
   * change falsified, and putting nothing in its place, is the exact shape of
   * fake gate this project keeps catching — it was *moved here*, to the moment
   * aiming actually happens.
   *
   * What is measured is the distance between **rendered target centres**, on
   * the boxes the engine laid out, with a real pointer on the middle dot. Not
   * the declared `--nc-rail-pitch-open`, not the ink's diameter, not a sum of
   * custom properties: the buttons' own rects, which is what a cursor has to
   * land inside. 26.75px is what it comes out at, and ≥24 is what is asserted,
   * because the point is the criterion's number and not this build's.
   *
   * **It is not a claim of conformance and must not be read as one.** 2.5.8 is
   * most defensibly evaluated on the target as presented, and as presented these
   * are 12px apart. The trade — density at rest, aim on approach — is written
   * up as a knowing departure on `.rail` in the stylesheet. This case binds the
   * half of it that is a measurable property; nothing binds the half that is a
   * judgement, which is why the judgement is written down instead.
   *
   * Red on the mutation that matters: with the spread reduced to what it was
   * before — the lift driving only the ink's size, `.railDot`'s `block-size`
   * back to a flat `var(--nc-rail-pitch)` — the dots stay 12px apart however
   * big their circles get, and both distances read 12.
   */
  it('opens at least 24px of aim between the hovered dot and its neighbours', async () => {
    await page.viewport(1400, 900);
    render(<RailPane turns={railTurns(12, 2)} />);
    await frame();
    await scrollPaneTo(0);
    await userEvent.hover(pane());
    await pause(150);

    const centre = (index: number) => {
      const box = dots()[index].getBoundingClientRect();
      return box.top + box.height / 2;
    };
    /* The premise, and the thing being bought: at rest they are a dense 12
       apart, which is under the criterion and is the reason this case has to
       exist at all. */
    expect(centre(6) - centre(5)).toBeCloseTo(12, 1);

    const aimed = dots()[5].getBoundingClientRect();
    await pointRailAt(aimed.top + aimed.height / 2);

    /* Both sides, because a spread that only pushed downward would give the
       reader 24px of aim on one flank and 12 on the other. */
    expect(centre(5) - centre(4)).toBeGreaterThanOrEqual(24);
    expect(centre(6) - centre(5)).toBeGreaterThanOrEqual(24);
    /* And the target itself is that tall, so the distance is real estate the
       cursor can be inside rather than a gap of nothing between two 12px
       slivers. */
    expect(dots()[5].getBoundingClientRect().height).toBeGreaterThanOrEqual(24);

    railTrack().dispatchEvent(new PointerEvent('pointerleave', { pointerType: 'mouse' }));
    await settle();
    await pause(150);
    /* It gives the density straight back: the aim is borrowed for as long as a
       pointer is there and no longer. */
    expect(centre(6) - centre(5)).toBeCloseTo(12, 1);
  });

  /*
   * ── The column does not slide out from under the pointer ──────────────────
   *
   * **Two failures, measured, one cause.** Growing the rows makes the column
   * longer, and where the extra length goes depends on how the track is
   * aligning its content — which is itself a function of the length. Measured
   * before the shoulders existed: on a column short enough to be centred, a dot
   * near either end of the list has a lopsided envelope (there are no dots past
   * the list to grow) and slid **12px**; on a column long enough to overflow,
   * `safe center` has degraded to `start`, nothing absorbs the growth above the
   * pointer, and the aimed dot slid **49px** — four resting pitches, far enough
   * that the pointer was no longer inside the dot it had been put on, which
   * then landed the envelope on a different dot.
   *
   * The fix is on the component: a shoulder of blank at each end that is
   * exactly the growth missing from its side, so growth + shoulder is a
   * constant and the column's length — and the distance from the track's top
   * edge to the pointer's own dot — stop depending on the pointer. This is that
   * property, asserted where it is hardest: both alignments, and at the ends of
   * the list where the envelope is clipped.
   *
   * Also asserted here because it is the same invariant seen from the side: the
   * track's `scrollHeight` does not move when a pointer arrives. That is what
   * makes "spreading cannot push a rail that fitted past the cap" true by
   * construction rather than by a clamp, and it is what stops `safe center`
   * flipping its alignment halfway through a hover.
   *
   * Red on: the shoulders removed; `--nc-rail-lead`/`--nc-rail-tail` published
   * as a constant instead of as the complement of the growth; and
   * `overflow-anchor: none` deleted, which hands the offset back to the
   * engine's own anchoring and reproduces the 49px slide almost exactly.
   */
  it('holds every dot still when the pointer arrives, at both alignments', async () => {
    await page.viewport(1400, 900);
    const centres = () => dots().map((each) => {
      const box = each.getBoundingClientRect();
      return box.top + box.height / 2;
    });
    const holdsStillAt = async (index: number) => {
      const before = centres();
      const extentBefore = railTrack().scrollHeight;
      const box = dots()[index].getBoundingClientRect();
      const y = box.top + box.height / 2;
      await pointRailAt(y);
      /* The dot the pointer was put on has not moved… */
      expect(centres()[index]).toBeCloseTo(before[index], 0);
      /* …and it is still the dot under the pointer, which is the thing the
         reader would actually notice going wrong. */
      const after = dots()[index].getBoundingClientRect();
      expect(y).toBeGreaterThanOrEqual(after.top);
      expect(y).toBeLessThanOrEqual(after.bottom);
      /* The column is the same length spread as at rest. */
      expect(railTrack().scrollHeight).toBe(extentBefore);
      railTrack().dispatchEvent(new PointerEvent('pointerleave', { pointerType: 'mouse' }));
      await settle();
      await pause(150);
    };

    /* Centred: eight dots in a 320px track. The ends are the interesting ones —
       the middle was already still by symmetry before the shoulders. */
    render(<RailPane turns={railTurns(8, 2)} />);
    await frame();
    await scrollPaneTo(0);
    await userEvent.hover(pane());
    await pause(200);
    expect(railTrack().scrollHeight).toBeLessThanOrEqual(railTrack().clientHeight + 1);
    for (const index of [0, 3, 6, 7]) await holdsStillAt(index);

    /* Start-aligned: forty dots, well past the cap, parked mid-scroll so there
       is column above and below the pointer. */
    document.body.replaceChildren();
    render(<RailPane turns={railTurns(40, 0)} paneHeight={400} />);
    await frame();
    await userEvent.hover(pane());
    await pause(200);
    railTrack().scrollTop = 100;
    await settle();
    expect(railTrack().scrollHeight).toBeGreaterThan(railTrack().clientHeight + 1);
    for (const index of [4, 12, 30]) await holdsStillAt(index);
    /* And the reader's own scroll position survived every one of them: a
       compensation written as a `scrollTop` nudge would have moved it. */
    expect(railTrack().scrollTop).toBe(100);
  });

  /*
   * ── The end dots stay reachable with the rail spread open ─────────────────
   *
   * The hazard is specific and this rail has already shipped its twin: a
   * scrollport whose content is pushed somewhere no gesture reaches. Spreading
   * adds `RAIL_SPREAD_SPAN` openings of length to the column, so the naive
   * version of this feature makes a rail that fitted overflow, and a rail that
   * already overflowed overflow further — and if any of that lands before the
   * scroll origin or after its end, the first or last exchange is simply gone
   * for as long as a pointer is on the rail.
   *
   * So both ends are asserted **while the spread is open**, which is the state
   * the existing `keeps the first and the last dot reachable…` case does not
   * cover: it runs at rest, and at rest there is nothing to push anything
   * anywhere. The two cases are the same claim at the two states, and both are
   * needed — this one passes trivially without the other, because a rail with
   * plain `center` fails at rest before it gets here.
   */
  it('keeps the end dots reachable while the rail is spread open', async () => {
    await page.viewport(1400, 900);
    render(<RailPane turns={railTurns(40, 0)} paneHeight={400} />);
    await frame();
    const track = railTrack();
    expect(dots()).toHaveLength(40);
    expect(track.scrollHeight).toBeGreaterThan(track.clientHeight + 1);

    /* A pointer parked in the middle of the list, so the spread is at its
       widest and pushing in both directions. */
    const aimed = dots()[20].getBoundingClientRect();
    await pointRailAt(aimed.top + aimed.height / 2);

    track.scrollTop = 0;
    await settle();
    const atTop = track.getBoundingClientRect();
    const first = dots()[0].getBoundingClientRect();
    expect(first.top).toBeGreaterThanOrEqual(atTop.top - 0.5);
    expect(first.bottom).toBeLessThanOrEqual(atTop.bottom + 0.5);

    track.scrollTop = track.scrollHeight;
    await settle();
    const atBottom = track.getBoundingClientRect();
    const last = dots()[39].getBoundingClientRect();
    expect(last.bottom).toBeLessThanOrEqual(atBottom.bottom + 0.5);
    expect(last.top).toBeGreaterThanOrEqual(atBottom.top - 0.5);

    railTrack().dispatchEvent(new PointerEvent('pointerleave', { pointerType: 'mouse' }));
    await settle();
    await pause(150);
  });

  /*
   * ── The resting shoulder is the stylesheet's fallback, and it has to be ───
   *
   * The shoulders are published by `public.tsx` as a multiple of the opening,
   * and the multiple at rest is `RAIL_SPREAD_SPAN ÷ 2`. But the component only
   * publishes from a pointer event, so on a rail no pointer has touched there
   * is no custom property at all and the stylesheet's own `var(…, 2)` fallback
   * *is* the layout. That number is the span halved, written in the other file,
   * with nothing but this case tying the two together: change the span to six
   * and the resting rail keeps a four-opening shoulder, so the column sits off
   * its own centre from first paint until the first hover and snaps when it
   * arrives.
   *
   * Measured off the untouched rail, against the opening the stylesheet
   * declares — so neither the fallback nor the opening can be edited alone.
   */
  it('rests on a shoulder of half the spread span, before any pointer', async () => {
    await page.viewport(1400, 900);
    render(<RailPane turns={railTurns(8, 2)} paneHeight={400} />);
    await frame();
    const track = railTrack();
    const number = (name: string) =>
      Number.parseFloat(getComputedStyle(track).getPropertyValue(name));
    const opening = number('--nc-rail-pitch-open') - number('--nc-rail-pitch');
    expect(opening).toBe(16);

    /* What the component publishes with no pointer anywhere near the rail — it
       runs one pass on mount, so this is a written value and the assertion
       below it is on the arithmetic that produced it. */
    expect(track.style.getPropertyValue('--nc-rail-lead')).toBe('2');
    expect(track.style.getPropertyValue('--nc-rail-tail')).toBe('2');
    /* Half of a span of four, times the opening: 2 × 16. */
    expect(Number.parseFloat(getComputedStyle(dots()[0]).marginBlockStart)).toBe(2 * opening);
    expect(Number.parseFloat(getComputedStyle(dots()[7]).marginBlockEnd)).toBe(2 * opening);

    /* **And the same again with the properties taken away**, which is the half
       this case exists for. The mount pass is not the first layout: there is a
       paint before it, and on it the stylesheet's `var(…, 2)` fallback is the
       whole of the geometry. Removing the published pair reproduces that frame
       exactly, and a fallback that had drifted from `RAIL_SPREAD_SPAN ÷ 2`
       would show up here as a column that jumps when the component catches up. */
    track.style.removeProperty('--nc-rail-lead');
    track.style.removeProperty('--nc-rail-tail');
    expect(Number.parseFloat(getComputedStyle(dots()[0]).marginBlockStart)).toBe(2 * opening);
    expect(Number.parseFloat(getComputedStyle(dots()[7]).marginBlockEnd)).toBe(2 * opening);
    /* And it really is a shoulder on the column rather than on the box the
       clamps measure against — the track is still exactly the cap. */
    expect(Math.round(track.getBoundingClientRect().height)).toBe(320);
    expect(getComputedStyle(track).paddingBlockStart).toBe('0px');
  });

  /*
   * ── A finger publishes no lift ────────────────────────────────────────────
   *
   * The stylesheet gates the magnification on `@media (pointer: fine)`, and on
   * a laptop with a touchscreen that query matches — so on the one device where
   * a finger and a mouse share a page, the media query is not the guard and the
   * component's `pointerType` check is the whole of it. Without it a tap leaves
   * a swell under wherever it landed and there is no second event to clear it.
   * Executed with the check removed: 35/35 green.
   *
   * Both pointer types go through the identical dispatch, so this cannot pass
   * by the events simply not arriving.
   */
  it('does not swell the dots for a touch pointer', async () => {
    await page.viewport(1400, 900);
    render(<RailPane turns={railTurns(12, 2)} />);
    await frame();
    await scrollPaneTo(0);
    await userEvent.hover(pane());
    await pause(150);
    const aimed = dots()[5].getBoundingClientRect();
    const at = aimed.top + aimed.height / 2;

    const move = (pointerType: string) => {
      railTrack().dispatchEvent(new PointerEvent('pointermove', {
        bubbles: true, pointerType, clientY: at,
      }));
    };

    move('touch');
    await settle();
    await pause(150);
    for (let index = 1; index < 12; index += 1) expect(dotInk(index)).toBeCloseTo(4, 1);
    for (const dot of dots()) expect(dot.style.getPropertyValue('--nc-dot-lift')).toBe('');

    /* And the same dispatch with a mouse does swell it, which is what makes the
       assertion above about the guard rather than about the plumbing. */
    move('mouse');
    await settle();
    await pause(150);
    expect(dotInk(5)).toBeCloseTo(8, 1);

    railTrack().dispatchEvent(new PointerEvent('pointerleave', { pointerType: 'mouse' }));
    await settle();
    await pause(150);
  });

  /*
   * ── The track's own scroll moves the dots under a stationary pointer ──────
   *
   * The track is a scrollport, so a wheel over the rail moves every dot while
   * the pointer stays where it is. Without a `scroll` trigger the envelope
   * stays where the dots used to be — a curve centred on nothing. Executed with
   * the listener removed: 35/35 green, against a comment arguing it is not
   * decorative.
   */
  it('re-centres the envelope when the track scrolls under the pointer', async () => {
    await page.viewport(1400, 900);
    render(<RailPane turns={promptTurns(30, () => 'Ask about the rewrite')} />);
    await frame();
    await scrollPaneTo(0);
    await userEvent.hover(pane());
    await pause(150);
    const track = railTrack();
    /* The premise: four pitches of scroll to give, so the dot that was at the
       peak ends up outside the span entirely. */
    const pitch = Number.parseFloat(getComputedStyle(track).getPropertyValue('--nc-rail-pitch'));
    expect(track.scrollHeight).toBeGreaterThan(track.clientHeight + pitch * 4);

    track.scrollTop = 0;
    await settle();
    const aimed = dots()[8].getBoundingClientRect();
    const at = aimed.top + aimed.height / 2;
    await pointRailAt(at);
    expect(dotInk(8)).toBeCloseTo(8, 1);

    track.scrollTop = pitch * 8;
    await settle();
    await pause(150);

    /*
     * The pointer did not move, so the peak is now on whichever dot moved under
     * it, and the one that was there is past the span and back at rest.
     *
     * **Which index that is, is read and not predicted.** It used to be
     * arithmetic — "four pitches of scroll, so four dots along" — and that
     * arithmetic is dead: while the rail is spread the rows are not all a pitch
     * tall, so a given number of pixels of scroll does not buy a fixed number of
     * dots. Locating the dot by its own box is the same claim without the
     * assumption, and it is a stricter one, because it fails if *no* dot is at
     * the peak rather than only if the wrong one is.
     */
    const under = dots().findIndex((dot) => {
      const box = dot.getBoundingClientRect();
      return at >= box.top && at <= box.bottom;
    });
    expect(under).toBeGreaterThan(8);
    /* Near the peak and *the* peak. Not `toBeCloseTo(8)`: the pointer is
       wherever the scroll left it relative to a row, not on a centre, so the
       dot under it is at 0.96 of full lift rather than at 1. Asserting the
       maximum is the claim anyway — "the envelope is centred here" — and it is
       the assertion a stale envelope fails, because a stale one has its maximum
       somewhere the pointer is not. */
    const inks = dots().map((_dot, index) => dotInk(index));
    expect(dotInk(under)).toBe(Math.max(...inks));
    expect(dotInk(under)).toBeGreaterThan(7.5);
    expect(dotInk(8)).toBeCloseTo(4, 1);

    railTrack().dispatchEvent(new PointerEvent('pointerleave', { pointerType: 'mouse' }));
    await settle();
    await pause(150);
  });

  /*
   * ── And a change of the exchange set moves them too ───────────────────────
   *
   * The envelope's effect installs once, so nothing in it fires when a turn
   * arrives or history is prepended: every dot moves under a pointer that has
   * not, and the lifts written for the old positions stay written. Measured
   * before the trigger: with the pointer on a centred dot and one exchange
   * prepended, that dot slid a whole pitch and kept a lift of `1` — and kept it
   * until the reader moved, scrolled, or left.
   *
   * History arrives in front of what is on screen, which is why the fixture
   * prepends rather than appends: appending moves nothing.
   */
  it('re-centres the envelope when the exchange set changes under the pointer', async () => {
    await page.viewport(1400, 900);
    const later = (index: number) => `later-${index}`;
    const turns = (from: number) => Array.from({ length: 12 - from }).flatMap((_u, index) => [
      { id: later(from + index), author: 'you' as const, text: `Ask ${from + index}`,
        atMs: (from + index) * 2_000 },
      { id: `agent-${from + index}`, author: 'agent' as const, text: 'Short.',
        atMs: (from + index) * 2_000 + 1 },
    ]);
    const { rerender } = render(<RailPane turns={turns(2)} />);
    await frame();
    await scrollPaneTo(0);
    await userEvent.hover(pane());
    await pause(150);
    expect(dots()).toHaveLength(10);

    const aimed = dots()[5].getBoundingClientRect();
    const at = aimed.top + aimed.height / 2;
    await pointRailAt(at);
    expect(dotInk(5)).toBeCloseTo(8, 1);

    /* Two exchanges of history land in front of everything. React keys the dots
       by exchange id, so the swollen *element* travels two positions down the
       track with its inline lift still on it, and the position the pointer is
       actually over is filled by a node that has never been written to. With
       nothing scheduling a frame the rail is then wrong in both places at once,
       and stays wrong until the reader moves. */
    rerender(<RailPane turns={turns(0)} />);
    await settle();
    await pause(150);

    expect(dots()).toHaveLength(12);
    /*
     * The pointer has not moved, so the peak is wherever the pointer is — and
     * **which index that is moved with the centring.** A top-anchored column
     * kept index 5 at the same y, so the peak stayed at 5. The column is
     * centred now, so two more exchanges is two more pitches of column and one
     * pitch of it is absorbed at the top: every dot slid up and the one under
     * that unchanged clientY is further down the list than it was.
     *
     * **Read off the boxes rather than named**, for the reason the case above
     * gives: while the rail is spread the rows are not a uniform pitch, so "one
     * pitch absorbed at the top, therefore index 6" is arithmetic that no
     * longer closes. What the case is actually about survives intact and is
     * asserted directly — the peak is under the pointer, and it is not on the
     * element that was carrying it before the prepend.
     */
    const under = dots().findIndex((dot) => {
      const box = dot.getBoundingClientRect();
      return at >= box.top && at <= box.bottom;
    });
    expect(under).toBeGreaterThan(5);
    const inks = dots().map((_dot, index) => dotInk(index));
    expect(dotInk(under)).toBe(Math.max(...inks));
    expect(dotInk(under)).toBeGreaterThan(7.5);
    /* And the dot the swell was carried onto — the same exchange, keyed by
       React, two positions further down the list — is off the peak rather than
       still stuck at it. A stale lift would leave that element at 8 while the
       dot under the pointer sat at rest, which is the failure in both places at
       once that this case was written for. */
    expect(dots()[7].getAttribute('aria-label')).toContain('Ask 7');
    expect(dotInk(7)).toBeLessThan(7.5);
    expect(dotInk(7)).toBeGreaterThan(4);
    /* An arc with its top on the pointer, which is what a stale lift is not: a
       stale one leaves a maximum on the element it was written to and a flat
       shoulder where the pointer actually is. Asserted as strict descent on
       both flanks rather than as an equality between them, because the pointer
       sits off-centre in its row after the prepend and the two flanks are
       genuinely — and correctly — a little different. */
    expect(dotInk(under)).toBeGreaterThan(dotInk(under - 1));
    expect(dotInk(under)).toBeGreaterThan(dotInk(under + 1));
    expect(dotInk(under - 1)).toBeGreaterThan(dotInk(under - 2));
    expect(dotInk(under + 1)).toBeGreaterThan(dotInk(under + 2));

    railTrack().dispatchEvent(new PointerEvent('pointerleave', { pointerType: 'mouse' }));
    await settle();
    await pause(150);
  });

  /*
   * ── And the same number of exchanges under different ids ──────────────────
   *
   * The case above changes the *count*, and the count is the only thing the
   * envelope's per-dot write cache watches: it throws the cache away when its
   * length stops matching the dot array's. So a list that keeps its length and
   * changes its ids walks straight past that guard. React keys the dots by
   * exchange id, so every button is unmounted and a fresh one mounted in its
   * slot — carrying no inline style, because inline styles are the effect's and
   * not the render's — while the cache still holds the lifts it wrote to the
   * elements that are gone. Every write is then skipped as a no-op against a
   * value nothing on the page has any more.
   *
   * Measured against the unfixed component, pointer parked on the sixth dot of
   * twelve: the arc `4 / 4.609 / 6 / 7.375 / 8 / 7.375 / 6 / 4.609 / 4` px and
   * its inline lifts `0.156 / 0.5 / 0.844 / 1 / …` were there before the swap
   * and **every one of them was gone after it** — all twelve dots flat at their
   * 4px rest, every `--nc-dot-lift` removed, and the 26.75px of aim the spread
   * had opened between two centres back down to the resting 12. It does not
   * heal on the next pointer move at the same y either, because that pass
   * computes the same lifts and skips the same writes.
   *
   * **The shoulders are not part of what this asserts, and the reason is worth
   * writing down.** `--nc-rail-lead`/`--nc-rail-tail` are published outside the
   * cache, so they come out the same either way — measured at `0` and `2.2e-16`
   * both before and after, because a fully interior envelope is exactly the
   * growth the shoulders exist to complement. That is the failure rather than
   * the alibi: the track is holding back four openings of blank in exchange for
   * a spread that is no longer on the page. What that is visible *as* is the
   * dots, which is what is asserted.
   *
   * Not currently reachable from the product — `ChatThread` is keyed by the
   * drawer's own id and exchange ids are server turn ids, so nothing swaps a
   * list of the same length for a different one — which is why the fixture
   * constructs it directly rather than driving it through a route.
   */
  it('keeps the envelope when the ids change under the pointer', async () => {
    await page.viewport(1400, 900);
    /* The same twelve exchanges twice over, differing in nothing a reader could
       see: identical prompts, identical replies, identical times. The geometry
       is therefore identical too, which is what makes this a clean test of the
       cache rather than of a relayout — the pointer is over the same row, and
       every lift the pass computes is the one it computed before. */
    const turns = (era: string) => Array.from({ length: 12 }).flatMap((_unused, index) => [
      { id: `${era}-you-${index}`, author: 'you' as const, text: `Ask ${index}`,
        atMs: index * 2_000 },
      { id: `${era}-agent-${index}`, author: 'agent' as const, text: 'Short.',
        atMs: index * 2_000 + 1 },
    ]);
    const { rerender } = render(<RailPane turns={turns('a')} />);
    await frame();
    await scrollPaneTo(0);
    await userEvent.hover(pane());
    await pause(150);
    expect(dots()).toHaveLength(12);

    const centre = (index: number) => {
      const box = dots()[index].getBoundingClientRect();
      return box.top + box.height / 2;
    };
    const aimed = dots()[5].getBoundingClientRect();
    await pointRailAt(aimed.top + aimed.height / 2);
    expect(dotInk(5)).toBeCloseTo(8, 1);

    rerender(<RailPane turns={turns('b')} />);
    await settle();
    await pause(150);

    /* Same length, and not one of the elements that carried the envelope. */
    expect(dots()).toHaveLength(12);
    expect(dots()[5].getAttribute('aria-label')).toContain('Ask 5');

    /* The envelope, still on the row the pointer never left: the peak, and the
       arc either side of it, which together are the claim a cache stuck on
       departed elements cannot satisfy — it leaves twelve dots at their rest
       and no maximum anywhere. */
    const inks = dots().map((_dot, index) => dotInk(index));
    /* Strictly above every *other* dot, not merely equal to the maximum of all
       of them. Under the defect this case is pointed at every dot is left at
       its rest, and the resting maximum is the lit dot's 6px — so `equal to the
       max` would have been satisfied by nothing more than the pointer happening
       to be parked on the lit row, and the case would have passed with the
       envelope gone. Measured with the cache working: dot 5 at 8, the next
       highest 7.375. */
    expect(dotInk(5)).toBeGreaterThan(Math.max(...inks.filter((_ink, index) => index !== 5)));
    expect(dotInk(5)).toBeCloseTo(8, 1);
    expect(dotInk(6)).toBeGreaterThan(dotInk(7));
    expect(dotInk(7)).toBeGreaterThan(dotInk(8));
    expect(dotInk(8)).toBeGreaterThan(dotInk(9));
    expect(dotInk(9)).toBeCloseTo(4, 1);
    /* And the aim the spread is for is still open, which is the same claim in
       the units a cursor cares about. */
    expect(centre(6) - centre(5)).toBeGreaterThanOrEqual(24);

    railTrack().dispatchEvent(new PointerEvent('pointerleave', { pointerType: 'mouse' }));
    await settle();
    await pause(150);
  });

  /*
   * ── Reduced motion drops the transition and keeps the sizes ───────────────
   *
   * The envelope is the aiming aid a 4px dot is bought with, so switching it
   * off under a motion preference would answer motion with a targeting
   * failure. What goes is the part that keeps moving after the input has
   * stopped: the transition.
   *
   * Asserted at the declaration and at its ordinal, which is now the *only*
   * case in this file that has to be — the coarse branch it used to share the
   * technique with is rendered in `thread.coarse.browser.test.tsx`, in a
   * browser context of its own. `prefers-reduced-motion` has no such context
   * yet: emulating the feature on this shared page poisons every file after it,
   * and the same `contextOptions` lever that solved the pointer split would
   * solve this one. Until then the declaration alone is not the behaviour, and
   * the block wins only by sitting after `@media (pointer: fine)`. Executed:
   * `transition: none` replaced with `inline-size 3s linear`, 35/35 green.
   */
  it('drops the dot transition under reduced motion, after the fine block', async () => {
    await page.viewport(1400, 900);
    render(<RailPane turns={railTurns(8)} />);
    await frame();
    /* A dot that is not the lit one, so `.railDotActive` is not in the classes
       the ledger is built from and the `::before` rules cannot be confused. */
    const ledger = ruleLedgerFor(dots()[1]).filter((entry) => entry.pseudo === '::before');
    const still = under(ledger, 'prefers-reduced-motion');
    const moving = under(ledger, 'pointer: fine');
    expect(still).toHaveLength(1);
    expect(moving).toHaveLength(1);

    expect(still[0].rule.style.transition).toBe('none');
    /* And it is the fine block's transition it is cancelling, which is the only
       reason the order matters. */
    expect(moving[0].rule.style.transition).not.toBe('');
    expect(still[0].at).toBeGreaterThan(moving[0].at);
  });

  /*
   * ── The prompt, after a pointer has rested on a dot ───────────────────────
   *
   * Three separable claims, and the delay is the one that is easiest to lose:
   * with it at zero the layer fires on every dot a pointer crosses on its way
   * to the composer, which is a strobe over the thing the reader is looking at.
   *
   * **This tier no longer pins the number, and the assertion below is not a
   * measurement of it.** It used to be: the wait was polled and the elapsed
   * time asserted into `(380, 650)`, which the shipped 450 sits in the middle
   * of and which 300 and 700 both fall outside. That left 200ms for the
   * driver's hover round-trip, the poll's own 20ms of resolution and the
   * runner's scheduling, and on a shared runner it ran out: run 33380223777
   * measured 671.6ms — 221.6ms of overhead on a 450ms delay — under a mutation
   * of the app's providers. Widening the band does not repair it, it disarms
   * it: the ceiling was the half that rejected 700, and the overhead runs one
   * way, so the same 221.6ms lifts a 300ms delay to 520ms and past the floor
   * that was rejecting *it*. The number is pinned on fake timers in
   * `public.test.tsx` instead — absent at 449, present at 450, no clock in it.
   *
   * What is left here needs a real engine and a real pointer, and neither half
   * is a millisecond claim: the panel is **not** up 150ms after the hover,
   * which is the strobe this delay exists to prevent, and it is up well inside
   * a ceiling on the feature being discoverable at all.
   */
  it('floats the prompt out only after the pointer has rested', async () => {
    await page.viewport(1400, 900);
    render(<RailPane turns={promptTurns(8, (index) => (index === 6 ? OVERLONG_PROMPT : LONG_PROMPT))} />);
    await frame();
    await scrollPaneTo(0);

    const startedAt = performance.now();
    await userEvent.hover(dots()[3]);
    await pause(150);
    expect(railPreview()).toBeNull();
    while (railPreview() === null && performance.now() - startedAt < 2_000) await pause(20);
    const shownAfter = performance.now() - startedAt;
    const preview = railPreview()!;
    expect(preview).not.toBeNull();
    /* A usability ceiling, not a band around the constant: past about this
       long nobody who merely paused on a 4px dot is still there. It leaves
       750ms over the shipped delay, three times the worst overhead on record
       (221.6ms). */
    expect(shownAfter).toBeLessThan(1_200);

    /* It carries **more** than the accessible name, which is the only reason a
       second rendering of the same fact is worth its ink: the name is capped so
       a screen reader does not read a paragraph to announce a button, and this
       is not announced. */
    const name = dots()[3].getAttribute('aria-label')!;
    expect(preview.textContent).toBe(LONG_PROMPT);
    expect(name.length).toBeLessThan(LONG_PROMPT.length);
    expect(name).toContain('…');

    /* And it is not a second channel: invisible to the accessibility tree, and
       not something a pointer can land on. */
    expect(preview.getAttribute('aria-hidden')).toBe('true');
    expect(getComputedStyle(preview).pointerEvents).toBe('none');

    /* **It opens back across the card, and containment changed hands.**
       The rail used to be in the transcript's leading gutter and the panel
       opened away from it, to its inline-end, over the prose — held in by the
       *pane's* own clip, which was the whole guarantee, since the component's
       clamp only bounds the panel's top and a tall panel overran the track.

       From the seam the only direction with room is back toward the card, so
       the panel opens inline-start and it is outside the pane altogether. What
       contains it now is the clamp, and the clamp finally *can*: the track's
       bottom is the seam's bottom is the card's bottom, so
       `min(wanted, trackBottom - height)` is measured against the drawer's own
       lower edge. Asserted against the seam, which is the box that changed. */
    const box = preview.getBoundingClientRect();
    const seamBox = document.querySelector<HTMLElement>('[data-nc-drawer-seam]')!
      .getBoundingClientRect();
    const railBox = railTrack().getBoundingClientRect();
    /* Beside the rail, on the card's side of it. */
    expect(box.right).toBeLessThanOrEqual(railBox.left);
    /* Over the card, not off the leading edge of the page. */
    const card = document.querySelector<HTMLElement>('[data-nc-drawer]')!.getBoundingClientRect();
    expect(box.left).toBeGreaterThanOrEqual(card.left);
    /* And it never turns the transcript into a horizontally scrolling one,
       because it is not in the transcript's scrollport at all. */
    expect(pane().contains(preview)).toBe(false);
    expect(pane().scrollWidth).toBeLessThanOrEqual(pane().clientWidth);
    /* Inside the drawer's block range, top and bottom — the containment the
       old note recorded as *not* holding. */
    expect(box.top).toBeGreaterThanOrEqual(seamBox.top - 1);
    expect(box.bottom).toBeLessThanOrEqual(seamBox.bottom + 1);

    /* **The cap, on the one exchange the fixture made too long for it.** 240 is
       a judgement about what can be glanced at, and it was the last number here
       nothing read: raised to 1000, this file stayed green.

       The rail is let back to rest before this dot is aimed at, and that is not
       tidiness. `userEvent.hover` teleports the cursor to wherever the target's
       box is *at the moment it is called* — and with the rail already spread
       around the fourth dot, the seventh dot's box is displaced by the rows
       between them. Landing there and then letting the envelope re-form around
       the new position moves that dot back to its resting place, out from under
       the cursor, and the panel that comes up belongs to a neighbour. From rest
       there is no displacement to be wrong about: the dot under the pointer
       does not move when the spread opens, which is the invariant `holds every
       dot still…` above is about. A real cursor never teleports, so this is the
       fixture paying for its own shortcut rather than a defect being avoided. */
    await userEvent.hover(replies()[0]);
    await pause(150);
    await userEvent.hover(dots()[6]);
    await pause(600);
    const capped = railPreview()!.textContent;
    expect(OVERLONG_PROMPT.length).toBeGreaterThan(240);
    expect(capped).toHaveLength(240);
    expect(capped.endsWith('…')).toBe(true);
    expect(OVERLONG_PROMPT.startsWith(capped.slice(0, -1))).toBe(true);

    /* Moving off the rail takes it with them. */
    await userEvent.hover(replies()[0]);
    await pause(150);
    expect(railPreview()).toBeNull();
  });

  /*
   * ── The clamp, at the only two places it does anything ────────────────────
   *
   * A panel centred on the first dot of a scrolled rail wants to start above
   * the track; one centred on the last wants to end below it. Both would then
   * be cut by the drawer's edge with half a sentence showing. The clamp is what
   * stops that — and it had no test: replaced with the unclamped `wanted`, this
   * file stayed green, because the case above hovers the fourth dot of eight,
   * where the panel is inside the track either way.
   *
   * **The end dots carry the overlong prompt on purpose, and that is new.** The
   * clamp only does anything when the panel is taller than twice the distance
   * from the track's edge to the dot's centre, and the spread's shoulder puts
   * the first dot 38px inside the track rather than hard against it. A
   * two-line panel therefore fits above it unclamped and this case measured
   * nothing — it read 17.7px of clearance against a clamp that would have given
   * 0. The overlong prompt *and* the narrow panel are what make it tall enough
   * for the clamp to be in play again — the panel's height is a function of its
   * width, so the prompt alone was not enough at the fixture's usual 396px and
   * still left 3.4px of clearance. Both together are the premise this case has
   * always needed and used to get for free from a first dot sitting on the
   * track's own edge. 240 is deliberately below the production floor: this
   * case owns the clamp's narrow-input behavior, not the shell's width policy.
   *
   * The premise is asserted rather than assumed: if the panel ever stops being
   * taller than the room above the first dot, this case goes back to measuring
   * nothing and should fail rather than pass.
   */
  it('holds the preview inside the track at the first dot and at the last', async () => {
    await page.viewport(1400, 900);
    render(<RailPane
      turns={promptTurns(30, (index) => (
        index === 0 || index === 29 ? OVERLONG_PROMPT : LONG_PROMPT
      ))}
      conversationSpan={240}
    />);
    await frame();
    await scrollPaneTo(0);
    const track = railTrack();
    /* The premise: a rail long enough that its own scrollport is in play. A
       track that holds every dot has no edges to clamp against. */
    expect(track.scrollHeight).toBeGreaterThan(track.clientHeight + 1);

    track.scrollTop = 0;
    await userEvent.hover(dots()[0]);
    await pause(600);
    const first = railPreview()!.getBoundingClientRect();
    const top = track.getBoundingClientRect();
    expect(first).not.toBeNull();
    /* The premise, stated as a number: unclamped, this panel would start above
       the track. Half its height is more than the room between the track's top
       edge and the dot's own centre, so `wanted` is negative and the clamp is
       the only thing putting it where it is. */
    const firstDot = dots()[0].getBoundingClientRect();
    expect(first.height / 2).toBeGreaterThan(firstDot.top + firstDot.height / 2 - top.top);
    expect(first.top).toBeGreaterThanOrEqual(top.top - 1);
    /* And it is really centred on nothing else: the clamp moved it, so it is
       *at* the edge rather than merely inside it. */
    expect(first.top).toBeCloseTo(top.top, 0);

    track.scrollTop = track.scrollHeight;
    await settle();
    await userEvent.hover(dots()[29]);
    await pause(600);
    const last = railPreview()!.getBoundingClientRect();
    const bottom = railTrack().getBoundingClientRect();
    expect(last.bottom).toBeLessThanOrEqual(bottom.bottom + 1);
    expect(last.bottom).toBeCloseTo(bottom.bottom, 0);

    await userEvent.hover(replies()[0]);
    await pause(150);
  });

  /*
   * ── The preview follows the track's own scroll ────────────────────────────
   *
   * The track is a scrollport, so a wheel over the rail moves the dot while the
   * panel — positioned against `.rail`, which does not scroll — stays where it
   * was. The envelope has a `scroll` listener for exactly this; the preview did
   * not, and its layout effect is keyed on the previewed id and the exchange
   * list, neither of which a scroll changes. Measured before the listener: 80px
   * of track scroll, 80px of separation, panel pointing at nothing.
   */
  it('keeps the preview on its dot when the rail scrolls under it', async () => {
    await page.viewport(1400, 900);
    /* A one-line prompt on purpose: this case is about the panel tracking its
       dot, and a panel tall enough to hit the clamp at either end would be
       measuring the clamp instead. The clamp has its own case above. */
    render(<RailPane turns={promptTurns(30, () => 'Ask about the rewrite')} />);
    await frame();
    await scrollPaneTo(0);
    const track = railTrack();
    expect(track.scrollHeight).toBeGreaterThan(track.clientHeight + 80);

    /* The preview is armed with a synthetic `pointerover` and the real cursor
       is parked off the rail, which is the only way to isolate what this case
       is about. With a real mouse resting on the rail, scrolling the track
       moves a *different* dot under the stationary cursor and the panel swaps
       to it — correct behaviour, and behaviour that would hide a panel that
       never moved on its own. */
    await userEvent.hover(pane());
    await pause(600);
    expect(railPreview()).toBeNull();
    track.scrollTop = 0;
    dots()[8].dispatchEvent(new PointerEvent('pointerover', {
      bubbles: true, pointerType: 'mouse',
    }));
    await pause(600);
    /* The premise: free of both edges before the scroll and after it, so what
       is asserted is the follow and not a clamp that happens to agree. */
    const clear = () => {
      const box = railPreview()!.getBoundingClientRect();
      const bounds = railTrack().getBoundingClientRect();
      return box.top > bounds.top + 1 && box.bottom < bounds.bottom - 1;
    };
    expect(clear()).toBe(true);
    const centred = () => {
      const box = railPreview()!.getBoundingClientRect();
      const dot = dots()[8].getBoundingClientRect();
      return (box.top + box.bottom) / 2 - (dot.top + dot.bottom) / 2;
    };
    expect(centred()).toBeCloseTo(0, 0);

    track.scrollTop = 80;
    await settle();

    expect(track.scrollTop).toBe(80);
    expect(clear()).toBe(true);
    expect(centred()).toBeCloseTo(0, 0);

    railTrack().parentElement!.dispatchEvent(new PointerEvent('pointerleave', {
      pointerType: 'mouse',
    }));
    await settle();
    await pause(150);
  });

  /*
   * ── The warm-up cannot be spent on a preview nobody saw ───────────────────
   *
   * "Once it is up, moving between dots swaps it with no second wait" was a
   * claim about a panel on screen, and the code tested `previewed`, which
   * records what was *armed*. A prompt that collapses to `''` arms it and
   * renders nothing — `railLabel` handles that case explicitly, so the codebase
   * already holds empty prompts to be reachable — and the next dot crossed then
   * flashed its panel immediately. Measured before the fix: 120ms after moving
   * on from the empty dot, against a 450ms delay, **shown**.
   */
  it('still waits the full delay after resting on a dot with nothing to show', async () => {
    await page.viewport(1400, 900);
    render(<RailPane turns={promptTurns(8, (index) => (index === 2 ? '   ' : LONG_PROMPT))} />);
    await frame();
    await scrollPaneTo(0);
    /* Park the real pointer off the rail: a mouse left on a dot by an earlier
       case arms a preview through a genuine boundary event. */
    await userEvent.hover(pane());
    await pause(600);
    expect(railPreview()).toBeNull();

    /* The empty dot exists and is named by its ordinal alone — the premise, so
       this cannot pass by the exchange simply not being there. */
    expect(dots()).toHaveLength(8);
    expect(dots()[2].getAttribute('aria-label')).toBe('Jump to exchange 3');

    await userEvent.hover(dots()[2]);
    await pause(600);
    expect(railPreview()).toBeNull();

    await userEvent.hover(dots()[4]);
    await pause(150);
    /* Nothing was ever up, so there is nothing to swap and the neighbour waits
       its own delay like the first one did. */
    expect(railPreview()).toBeNull();
    await pause(450);
    expect(railPreview()!.textContent).toBe(LONG_PROMPT);

    /* And the asymmetry the delay is written for is intact: *now* a panel is
       up, so the next dot swaps with no second wait. */
    await userEvent.hover(dots()[5]);
    await pause(120);
    expect(railPreview()).not.toBeNull();

    await userEvent.hover(replies()[0]);
    await pause(150);
  });

  /*
   * ── A finger is not a hover ───────────────────────────────────────────────
   *
   * A touchscreen's first pointer event on a control is the press, so a preview
   * armed from one appears *after* the reader has already been taken somewhere
   * — a panel describing where they no longer are. The stylesheet says the same
   * thing declaratively (`@media (pointer: coarse)`), but a laptop with a
   * touchscreen reports `pointer: fine`, so the media query is not the guard on
   * that device and the component's own check is.
   *
   * Both pointer types go through the identical dispatch, so this cannot pass
   * by the events simply not arriving.
   */
  it('does not float the prompt out for a touch pointer', async () => {
    await page.viewport(1400, 900);
    render(<RailPane turns={promptTurns()} />);
    await frame();
    /* Park the *real* pointer somewhere harmless first. The engine fires
       boundary events when the element under a stationary cursor changes, so a
       previous case that left the mouse on the rail would arm a preview here
       through a genuine mouse `pointerover` and this case would pass or fail on
       something it is not about. Asserted, so the parking is not merely
       hopeful. */
    await userEvent.hover(pane());
    await pause(600);
    expect(railPreview()).toBeNull();

    const enter = (pointerType: string) => {
      dots()[3].dispatchEvent(new PointerEvent('pointerover', { bubbles: true, pointerType }));
    };

    enter('touch');
    await pause(600);
    expect(railPreview()).toBeNull();

    enter('mouse');
    await pause(600);
    expect(railPreview()).not.toBeNull();
  });

  /*
   * ── The rule: the last exchange that has scrolled past the top ────────────
   *
   * At the very start of the transcript nothing has, so it is the first.
   */
  it('lights the first dot at the top of the transcript', async () => {
    await page.viewport(1400, 900);
    render(<RailPane turns={railTurns(8)} />);
    await frame();
    await scrollPaneTo(0);
    expect(currentDot()).toBe(0);
  });

  /*
   * ── A transcript shorter than its pane ────────────────────────────────────
   *
   * This is the first thing the rail ever does for anyone: it appears at five
   * exchanges, and five short exchanges do not fill the drawer. Nothing can be
   * scrolled past in a pane that cannot scroll, so the answer is the first dot
   * — and it stays the first dot when another one is pressed, because the press
   * writes a `scrollTop` the engine clamps to the one it is already at and the
   * pane does not move.
   *
   * Both halves were wrong before, from one cause. The rule took its
   * end-of-scroll branch here (`scrollHeight - scrollTop - clientHeight` is
   * zero at the *top* of a transcript that fits), so it lit the last dot on a
   * conversation nobody had begun reading; and the re-read after a jump then
   * put that answer straight back over the press, one frame later.
   *
   * The press is also the one path on which that re-read is the only thing
   * running: a clamped write moves nothing, so the browser dispatches no
   * `scroll`, so nothing else would ever correct it.
   */
  it('lights the first dot on a transcript that fits, and a press does not overrule it', async () => {
    await page.viewport(1400, 900);
    render(<RailPane turns={railTurns(5, 0)} paneHeight={800} />);
    await frame();
    const scroller = pane();
    /* The precondition, asserted rather than assumed: without it this test is
       only a second copy of "lights the first dot at the top". */
    expect(scroller.scrollHeight).toBeLessThanOrEqual(scroller.clientHeight);
    expect(dots()).toHaveLength(5);
    expect(currentDot()).toBe(0);

    dots()[3].click();
    await settle();

    expect(scroller.scrollTop).toBe(0);
    expect(currentDot()).toBe(0);
  });

  it('lights the dot for an exchange scrolled exactly to the top', async () => {
    await page.viewport(1400, 900);
    render(<RailPane turns={railTurns(8)} />);
    await frame();
    await scrollPaneTo(0);
    await scrollPaneTo(pane().scrollTop + markers()[5].getBoundingClientRect().top
      - pane().getBoundingClientRect().top);
    expect(currentDot()).toBe(5);
  });

  /*
   * **The next question peeking in at the bottom does not steal the mark.**
   *
   * Only your line carries a marker; the reply that follows it is a sibling and
   * is not part of the observed box at all. So with exchange 2's answer filling
   * the pane and exchange 3's question just entering from below, the old
   * "topmost intersecting marker" rule said 3 — while every word on screen
   * belonged to 2.
   */
  it('stays on the exchange being read while the next one peeks in at the bottom', async () => {
    await page.viewport(1400, 900);
    render(<RailPane turns={railTurns(8)} />);
    await frame();
    await scrollPaneTo(0);
    /* Put exchange 3's opening line five pixels above the pane's bottom edge:
       visible, and nowhere near read. */
    await scrollPaneTo(pane().scrollTop + markers()[3].getBoundingClientRect().top
      - pane().getBoundingClientRect().bottom + 5);

    expect(markers()[3].getBoundingClientRect().top)
      .toBeLessThan(pane().getBoundingClientRect().bottom);
    expect(currentDot()).toBe(2);
  });

  /*
   * **An exchange taller than the pane keeps the mark while you read its
   * middle.** Here no marker is inside the pane at all, so the observer this
   * replaced never fired and the mark froze wherever it had last been left.
   */
  it('keeps the mark on an exchange taller than the pane', async () => {
    await page.viewport(1400, 900);
    /* A short pane, so one exchange really is taller than it — which is the
       whole input. At the drawer's own height the same thing happens with a
       reply of a few hundred words. */
    render(<RailPane turns={railTurns(8)} paneHeight={220} />);
    await frame();
    await scrollPaneTo(0);
    /* Into the middle of exchange 2's answer: its own question is 100px above
       the pane's top edge, and the next question is below the bottom. */
    await scrollPaneTo(pane().scrollTop + markers()[2].getBoundingClientRect().top
      - pane().getBoundingClientRect().top + 100);

    /* The precondition, asserted rather than assumed — this test is worthless
       if any marker is on screen, because then it is only re-testing the case
       above. */
    const paneBox = pane().getBoundingClientRect();
    for (const marker of markers()) {
      const top = marker.getBoundingClientRect().top;
      expect(top < paneBox.top || top > paneBox.bottom).toBe(true);
    }
    expect(currentDot()).toBe(2);
  });

  /*
   * **A press near the end lights the dot that was pressed.** The browser clamps
   * the scroll to its maximum, so the pressed exchange never reaches the top;
   * the old rule then overruled the press with an earlier dot — measured on this
   * exact fixture by pinning the edge to the pane's top: press the ninth dot,
   * the sixth lights. At the end of the scroll the edge has slid to the pane's
   * bottom instead, which is the only one that can still tell the trailing
   * exchanges apart. (The indices below are the code's, from zero; the ordinals
   * in this sentence are the reader's, from one.)
   */
  it('lights the pressed dot even where the scroll clamps at the end', async () => {
    await page.viewport(1400, 900);
    render(<RailPane turns={railTurns(9, 6)} />);
    await frame();
    await scrollPaneTo(0);

    dots()[8].click();
    await settle();

    const scroller = pane();
    expect(scroller.scrollTop).toBe(scroller.scrollHeight - scroller.clientHeight);
    expect(currentDot()).toBe(8);
  });

  /*
   * ── …and it gets there without jumping ────────────────────────────────────
   *
   * The rule interpolates between two edges a whole pane-height apart, so the
   * question is not *whether* it reaches the bottom edge but whether it can get
   * there in one step. It could: with a threshold instead of a slide, one pixel
   * short of the maximum lit the ninth dot on this fixture and two pixels short
   * lit the sixth — three exchanges, back and forth, on the last screen of
   * every long conversation, and a trackpad delivers deltas that small
   * continuously.
   *
   * So what is asserted is the shape of the property rather than one offset:
   * over the last dozen pixels of scroll the mark never moves by more than one
   * exchange per pixel. It is still thirteen samples and not a proof — the
   * discontinuity it was written against lived in exactly this window, which is
   * what makes it the right window, and the *other* one this rail has had (a
   * transcript overflowing by a single pixel starting at the wrong edge) is
   * nowhere near it and is asserted separately below. A test that only checked
   * the endpoint could not see either, which is why the case above could not.
   */
  it('moves the mark at most one exchange per pixel through the end of the scroll', async () => {
    await page.viewport(1400, 900);
    render(<RailPane turns={railTurns(9, 6)} />);
    await frame();
    const scroller = pane();
    const max = scroller.scrollHeight - scroller.clientHeight;

    const seen: number[] = [];
    for (let back = 0; back <= 12; back += 1) {
      await scrollPaneTo(max - back);
      seen.push(currentDot());
    }

    expect(seen[0]).toBe(8);
    for (let step = 1; step < seen.length; step += 1) {
      expect(Math.abs(seen[step] - seen[step - 1])).toBeLessThanOrEqual(1);
    }
  });

  /*
   * ── The tab stop is the exchange you are in, and goes on being it ─────────
   *
   * `onFocus` moves the roving stop, and a mouse press fires `focus` too. With
   * nothing giving that state back, the component's own claim — "which one
   * holds the stop is the one you are reading, so Tab-then-Enter goes where you
   * already are" — was true until the reader's first click and false for the
   * rest of the session: measured before this, click the second dot, scroll to
   * the end, and the ninth dot is lit while the stop is still on the second.
   */
  it('gives the tab stop back to the lit dot when the lit dot moves', async () => {
    await page.viewport(1400, 900);
    render(<RailPane turns={railTurns(9, 6)} />);
    await frame();
    await scrollPaneTo(0);
    const stops = () => dots().map((dot) => dot.getAttribute('tabindex'));
    expect(stops()[0]).toBe('0');

    /* A press, as a pointer makes one: Chromium focuses the button it is
       pressing, and it is that focus — not the arrows — which moved the stop
       and then kept it. `HTMLElement.click()` alone does *not* focus, so a test
       that only clicked could not see this at all. */
    dots()[1].focus();
    dots()[1].click();
    await settle();
    expect(currentDot()).toBe(1);
    expect(stops()[1]).toBe('0');

    await scrollPaneTo(pane().scrollHeight);

    expect(currentDot()).toBe(8);
    expect(stops()[8]).toBe('0');
    expect(stops().filter((stop) => stop === '0')).toHaveLength(1);
  });

  /*
   * And the half the case above cannot see — which is the half the reset is
   * actually for.
   *
   * That case focuses dot 2 before pressing it, so the rail is still holding
   * focus when the lit dot moves, and the stop is carried by the effect's
   * **focus transfer** rather than by `setRoved(null)`: delete the reset and
   * that case stays green, named for it or not. The reset's own path is this
   * one — the reader pressed a dot and then went back to the transcript, so
   * focus has left the rail entirely and there is no focused dot for the
   * transfer to move the stop to. It is also the path the component's note
   * records as the measured original bug: click the second dot, scroll to the
   * end, and the ninth dot is lit while the stop is still on the second.
   */
  it('gives the tab stop back when the reader has left the rail', async () => {
    await page.viewport(1400, 900);
    render(<RailPane turns={railTurns(9, 6)} />);
    await frame();
    await scrollPaneTo(0);
    const stops = () => dots().map((dot) => dot.getAttribute('tabindex'));

    dots()[1].focus();
    dots()[1].click();
    await settle();
    expect(currentDot()).toBe(1);
    expect(stops()[1]).toBe('0');

    /* Back to reading. The pane is not focusable of its own accord, so it is
       made a target here; what the case needs is only that `activeElement` is
       somewhere outside the rail when the lit exchange changes. */
    const scroller = pane();
    scroller.setAttribute('tabindex', '-1');
    scroller.focus();
    expect(railTrack().contains(document.activeElement)).toBe(false);

    await scrollPaneTo(scroller.scrollHeight);

    expect(currentDot()).toBe(8);
    expect(stops()[8]).toBe('0');
    expect(stops().filter((stop) => stop === '0')).toHaveLength(1);
  });

  /*
   * ── A new turn does not take the pane away from the reader ────────────────
   *
   * The write used to fire on every change of `turns.length`, so a jump
   * followed by any append put the reader back at the bottom — this test's own
   * `parked` offset is the before, and `scrollHeight - clientHeight` was the
   * after. A live turn appends an activity line per action, so the rail's whole
   * purpose — read something earlier while the agent works — lasted one poll.
   */
  it('leaves the pane where the reader put it when a turn arrives', async () => {
    await page.viewport(1400, 900);
    const { rerender } = render(<RailPane turns={railTurns(8)} />);
    await frame();
    await scrollPaneTo(0);
    dots()[2].click();
    await settle();
    const parked = pane().scrollTop;
    expect(parked).toBeGreaterThan(0);

    rerender(<RailPane turns={[...railTurns(8), {
      id: 'late', author: 'agent' as const, text: `Later. ${LINE.repeat(4)}`, atMs: 99_000,
    }]} />);
    await settle();

    expect(pane().scrollTop).toBe(parked);
    expect(currentDot()).toBe(2);
  });

  /* And the other half, or the rule would be "never follow": a reader sitting
     at the end of the transcript still rides it down as it grows. */
  it('follows a new turn for a reader still at the end', async () => {
    await page.viewport(1400, 900);
    const { rerender } = render(<RailPane turns={railTurns(8)} />);
    await frame();
    await scrollPaneTo(pane().scrollHeight);

    rerender(<RailPane turns={[...railTurns(8), {
      id: 'late', author: 'agent' as const, text: `Later. ${LINE.repeat(4)}`, atMs: 99_000,
    }]} />);
    await settle();

    const scroller = pane();
    expect(scroller.scrollTop).toBe(scroller.scrollHeight - scroller.clientHeight);
  });

  /*
   * ── A pane that grows moves the reader without a scroll event ─────────────
   *
   * "Is the reader at the bottom" is answered from the pane's own `scroll`
   * events, and a resize does not dispatch one: the composer shrinking as a
   * draft is sent, or the window growing, closes the distance to the bottom
   * silently. Left to the scroll listener alone the flag stayed `false` for a
   * reader now plainly at the end, and live output stopped following for the
   * rest of the conversation.
   */
  it('follows again after the pane grows around a parked reader', async () => {
    await page.viewport(1400, 900);
    const { rerender } = render(<RailPane turns={railTurns(8)} paneHeight={400} />);
    await frame();
    const remaining = () => pane().scrollHeight - pane().scrollTop - pane().clientHeight;
    await scrollPaneTo(pane().scrollHeight - pane().clientHeight - 300);
    /* Decisively away: three hundred pixels is far outside the 64px that still
       counts as reading the newest turn. */
    expect(remaining()).toBeCloseTo(300, 0);

    rerender(<RailPane turns={railTurns(8)} paneHeight={650} />);
    await settle();
    /* Inside it now, and no scroll event said so — the reader never touched
       the wheel and `scrollTop` is untouched. */
    expect(remaining()).toBeLessThanOrEqual(64);

    rerender(<RailPane turns={[...railTurns(8), {
      id: 'late', author: 'agent' as const, text: `Later. ${LINE.repeat(4)}`, atMs: 99_000,
    }]} paneHeight={650} />);
    await settle();

    const scroller = pane();
    expect(scroller.scrollTop).toBe(scroller.scrollHeight - scroller.clientHeight);
  });

  /*
   * ── Every dot is reachable ────────────────────────────────────────────────
   *
   * The rail used to be as tall as its dots inside a pane that clipped it, and
   * sticky positioning moved it *with* the pane — so past `(pane − 8) ÷ pitch`
   * dots there was no scroll anywhere that brought the rest into view. Measured
   * in this 400px pane at the 28px pitch of the day: 14 of them. (The same
   * arithmetic at today's 24px gives 16. A denser rail pushes the tail further
   * out and does not remove it, which is why the scrollport is still the fix
   * and this is still the case that pins it.) The conversation long enough to
   * want a jump list was the conversation whose jump list did not work.
   */
  it('bounds the rail by the pane and scrolls the lit dot into its own view', async () => {
    await page.viewport(1400, 900);
    render(<RailPane turns={railTurns(40, 8)} />);
    await frame();
    const scroller = pane();
    const track = railTrack();
    expect(dots()).toHaveLength(40);

    /* Bounded: the rail cannot be taller than the pane it sticks inside. */
    expect(track.getBoundingClientRect().height).toBeLessThanOrEqual(scroller.clientHeight);
    /* And it really is a scrollport, not a clipped box: there is more rail than
       there is room for it. */
    expect(track.scrollHeight).toBeGreaterThan(track.clientHeight + 1);

    /* The dot for the end of the conversation is inside that scrollport once it
       is the one you are in — which is the whole of what "reachable" means. */
    await scrollPaneTo(scroller.scrollHeight);
    expect(currentDot()).toBe(39);
    const last = dots()[39].getBoundingClientRect();
    const box = track.getBoundingClientRect();
    expect(last.top).toBeGreaterThanOrEqual(box.top - 1);
    expect(last.bottom).toBeLessThanOrEqual(box.bottom + 1);

    /* And back the other way, so this is not "the rail happens to end there". */
    await scrollPaneTo(0);
    expect(currentDot()).toBe(0);
    const first = dots()[0].getBoundingClientRect();
    const back = track.getBoundingClientRect();
    expect(first.top).toBeGreaterThanOrEqual(back.top - 1);
    expect(first.bottom).toBeLessThanOrEqual(back.bottom + 1);

    /* **And the track itself is inside the drawer**, at the one offset where
       that is not free: unscrolled, the rail is still at its flow position
       below the drawer's 36px of clearance, and a track sized by the pane's
       *height* rather than by the room under its own top edge hangs that
       difference off the bottom of the pane — where the arithmetic above,
       which compares against the track's own unclipped box, cannot see it. */
    expect(back.bottom).toBeLessThanOrEqual(scroller.getBoundingClientRect().bottom + 1);
  });

  /*
   * ── A shorter pane can put the lit dot outside the track ──────────────────
   *
   * The track is bounded by the room under it, so a drawer that shrinks
   * shortens it — and a dot sitting just inside the old lower edge is then
   * outside the new one *with the lit exchange unchanged*. Nothing keyed on the
   * lit exchange re-runs, which is exactly why this needs its own trigger and
   * its own assertion: with the height re-published but the visibility check
   * left keyed on the active index alone, this file stayed green.
   */
  it('brings the lit dot back into view when the pane shrinks under it', async () => {
    await page.viewport(1400, 900);
    const { rerender } = render(<RailPane turns={railTurns(40)} paneHeight={400} />);
    await frame();
    await scrollPaneTo(0);
    /* Deep enough into the conversation that the lit dot is at the *bottom* of
       the track: the rail scrolls forward by the smallest write that shows it,
       so the dot it has just caught up with sits on that edge.

       **Which dot that is moved with the pitch, and the fixture moved with
       it.** At 28px the track held 14 dots and the twentieth was already past
       the edge; at 16 it holds 24, so the twentieth is comfortably inside the
       track and `keepInRailView` has nothing to do — the setup assertion below
       would fail and the case would be about a dot that was never out of view.
       The thirtieth is the same position under the new geometry. `railTurns(40)`
       rather than `railTurns(40, 8)` for the same reason: the thirtieth marker
       has to be able to reach the pane's top, which needs transcript under it,
       and thirty-two of the old fixture's forty replies were one word. */
    await scrollPaneTo(pane().scrollTop + markers()[30].getBoundingClientRect().top
      - pane().getBoundingClientRect().top);
    expect(currentDot()).toBe(30);
    expect(dots()[30].getBoundingClientRect().bottom)
      .toBeCloseTo(railTrack().getBoundingClientRect().bottom, 0);

    rerender(<RailPane turns={railTurns(40)} paneHeight={340} />);
    await settle();

    /* The lit exchange did not change — that is the whole point of the case.
       Nothing keyed on it re-runs, and the dot that was on the old lower edge
       is 60px past the new one. */
    expect(currentDot()).toBe(30);
    const dot = dots()[30].getBoundingClientRect();
    const box = railTrack().getBoundingClientRect();
    expect(dot.top).toBeGreaterThanOrEqual(box.top - 1);
    expect(dot.bottom).toBeLessThanOrEqual(box.bottom + 1);
    /* And "inside the track" is only worth anything while the track is inside
       the drawer. */
    expect(box.bottom).toBeLessThanOrEqual(pane().getBoundingClientRect().bottom + 1);
  });

  /*
   * ── An observation of a pane with no layout is not an observation ─────────
   *
   * The zero-height guard used to be at the effect's installation only, and the
   * `ResizeObserver` reports zero-height observations too — a `display: none`
   * ancestor produces one. Measured before the guard moved into the read: while
   * hidden, the mark jumped to the last exchange and the track's bound resolved
   * to `0px`. It healed on the next non-zero frame, which is precisely what
   * makes it the kind of thing no other assertion here would ever notice.
   */
  it('ignores an observation of a pane with no layout at all', async () => {
    await page.viewport(1400, 900);
    render(<RailPane turns={railTurns(40, 8)} />);
    await frame();
    await scrollPaneTo(0);
    expect(currentDot()).toBe(0);
    const bound = getComputedStyle(railTrack()).blockSize;
    expect(Number.parseFloat(bound)).toBeGreaterThan(0);

    const scroller = pane();
    scroller.style.display = 'none';
    await settle();

    /* Asserted **while it is hidden**, which is the only moment this is
       visible at all: the next non-zero observation puts both back, so a test
       that only looked afterwards would pass against the failure it is for. */
    expect(currentDot()).toBe(0);
    expect(getComputedStyle(railTrack()).blockSize).toBe(bound);

    scroller.style.display = '';
    await settle();

    expect(currentDot()).toBe(0);
    expect(getComputedStyle(railTrack()).blockSize).toBe(bound);
  });

  /*
   * ── A transcript that overflows by less than one pane ─────────────────────
   *
   * The band between "fits exactly" and "two panes tall", which is where most
   * five-to-ten-exchange conversations in this drawer actually live. The slide
   * that bends the rule at the end of the scroll is keyed on the *remaining*
   * scroll, and at the top of a transcript that overflows by 60px the remaining
   * scroll is 60px — so a slide keyed on that number alone is already
   * `paneHeight − 60` of the way down before the reader has touched anything.
   * Measured before the cap on this exact fixture: the fifth dot lit at the top,
   * and pressing the first lit the sixth, which is the r1 failure ("press dot 1,
   * dot 6 lights") surviving as a band where the committed case above only
   * nailed the point `overflow === 0`.
   *
   * The pane is sized off the transcript's own measured height rather than
   * guessed at, because the whole case is a relationship between the two.
   */
  it('lights the first dot at the top of a transcript that overflows by less than a pane', async () => {
    await page.viewport(1400, 900);
    render(<RailPane turns={railTurns(6, 0)} paneHeight={2000} />);
    await frame();
    /* The transcript's own box, not the pane's `scrollHeight`: a pane taller
       than its content reports its own height for that. */
    const content = document.querySelector<HTMLElement>('[data-nc-rail-pane-inner]')!
      .getBoundingClientRect().height;

    document.body.replaceChildren();
    render(<RailPane turns={railTurns(6, 0)} paneHeight={content - 60} />);
    await frame();

    /* The precondition, asserted rather than assumed: this is worthless unless
       the transcript really is in the band. */
    const scroller = pane();
    const overflow = scroller.scrollHeight - scroller.clientHeight;
    expect(overflow).toBeGreaterThan(0);
    expect(overflow).toBeLessThan(scroller.clientHeight);
    expect(dots()).toHaveLength(6);

    /* The conversation opened at its newest turn, i.e. 60px down. Scrolling
       back to the start is the gesture the whole rail exists for, and it is the
       one that made this visible. */
    expect(scroller.scrollTop).toBe(overflow);
    await scrollPaneTo(0);
    expect(currentDot()).toBe(0);

    /* And a press cannot overrule it either: the write is clamped to the 60px
       of scroll that exist, which is not enough to bring the second exchange to
       any edge, so the honest answer is still the first. */
    dots()[1].click();
    await settle();
    expect(currentDot()).toBe(0);
  });

  /*
   * ── A pane that mounts with no layout gets its rail when it gains one ─────
   *
   * The zero-height guard sat at the top of the effect, before the listener and
   * the observer went on — so a drawer that mounted at zero height had nothing
   * left that could notice it growing. Nothing re-runs that effect for a change
   * of *height* (its dependencies are the set of exchanges), so the rail stayed
   * dead for the life of the conversation: measured at 0 → 400px followed by a
   * real scroll, no dot was ever lit. (That measurement also recorded that
   * `--nc-rail-room` was never published; the room is a CSS fact now and there
   * is nothing left to publish, so only the "no dot was ever lit" half of it
   * still describes anything.)
   */
  it('lights the rail after a pane that mounted with no layout gains some', async () => {
    await page.viewport(1400, 900);
    const { rerender } = render(<RailPane turns={railTurns(8)} paneHeight={0} />);
    await frame();
    expect(pane().clientHeight).toBe(0);
    /* Nothing is lit, and that is right: there is no box to read. */
    expect(currentDot()).toBe(-1);

    rerender(<RailPane turns={railTurns(8)} paneHeight={400} />);
    await settle();

    /* The observation of the pane gaining its height is the only thing that
       could have answered — the exchanges did not change, so the effect that
       installs all this did not re-run. The conversation opens at its newest
       turn, so the answer is the last dot rather than the first. */
    expect(currentDot()).toBe(7);
    expect(Number.parseFloat(getComputedStyle(railTrack()).blockSize)).toBeGreaterThan(0);

    /* And the scroll listener is live too, not merely the observer. */
    await scrollPaneTo(0);
    expect(currentDot()).toBe(0);
    await scrollPaneTo(pane().scrollTop + markers()[3].getBoundingClientRect().top
      - pane().getBoundingClientRect().top);
    expect(currentDot()).toBe(3);
  });

  /*
   * ── The focus goes where the tab stop goes ────────────────────────────────
   *
   * Giving the stop back to the lit dot moves `tabIndex="0"` out from under a
   * dot that may be holding DOM focus, and a roving group whose focused element
   * is not its tab stop is broken in the one way the pattern exists to prevent:
   * the reader's next Tab leaves from a `tabIndex="-1"` element. Measured
   * before this — focus the second dot, scroll the pane to the end — the stop
   * was dot 9 while `document.activeElement` was still dot 2.
   */
  it('moves the focus with the tab stop when the rail is holding it', async () => {
    await page.viewport(1400, 900);
    render(<RailPane turns={railTurns(9, 6)} />);
    await frame();
    await scrollPaneTo(0);
    dots()[1].focus();
    expect(document.activeElement).toBe(dots()[1]);

    await scrollPaneTo(pane().scrollHeight);

    expect(currentDot()).toBe(8);
    expect(document.activeElement).toBe(dots()[8]);
    expect(dots()[8].getAttribute('tabindex')).toBe('0');
    /* And it is the same fact from the other side: no dot in the ring is
       focused while a different one holds the stop. */
    expect(dots().filter((dot) => dot.getAttribute('tabindex') === '0')).toHaveLength(1);
  });

  /* The other half: a rail that is not holding focus never takes it. A reader
     scrolling with the wheel, or typing in the composer, must not have the
     caret pulled onto a navigation dot because the mark moved under them. */
  it('does not take focus when the rail is not holding it', async () => {
    await page.viewport(1400, 900);
    render(<RailPane turns={railTurns(9, 6)} />);
    await frame();
    await scrollPaneTo(0);
    const elsewhere = document.createElement('button');
    document.body.append(elsewhere);
    elsewhere.focus();

    await scrollPaneTo(pane().scrollHeight);

    expect(currentDot()).toBe(8);
    expect(document.activeElement).toBe(elsewhere);
    elsewhere.remove();
  });

  /*
   * ── The rail enters and leaves as part of the card ────────────────────────
   *
   * **This is the one case in the file that drives the real `<Drawer>`**, and
   * it has to. Every case above builds the drawer's geometry from the drawer's
   * own classes but supplies the boxes itself, which is right for measuring
   * geometry and cannot say anything about the *lifecycle* — that the drawer
   * renders a seam at all, that `ChatThread` finds it through
   * `drawerSeamAround`, that the two boxes animate together, and that closing
   * takes the rail with it. All four are production wiring, and a fixture that
   * hand-rolls the seam proves none of them.
   *
   * Three claims, and the third is the one a reader would notice:
   *
   *   1. The rail is in the drawer's own seam, found by the component with no
   *      help from this file.
   *   2. Card and seam carry the **same** animation — same name, duration and
   *      timing function, on both the enter and the exit. Read off the engine
   *      and compared to each other rather than to literals, because what must
   *      hold is that they move as one object; a card that slid while the dots
   *      sat still, or the reverse, is the visible failure here and it is
   *      exactly what a seam given its own timing would produce.
   *   3. Closing removes the rail. Asserted after the exit animation has run to
   *      completion, so a rail that merely stayed put through the fade — the
   *      dots hanging in the page beside a card that has gone — is red.
   */
  it('enters and leaves with the drawer, and takes the dots with it', async () => {
    await page.viewport(1400, 900);
    const host = document.createElement('div');
    host.style.cssText = 'position:relative;block-size:600px;inline-size:900px';
    document.body.append(host);

    function Harness({ open }: { open: boolean }) {
      return (
        <Drawer open={open} title="Ship the rewrite" onClose={() => {}}>
          <ChatThread conversation={railConversation()} turns={railTurns(8)} />
        </Drawer>
      );
    }
    const view = render(<Harness open />, { container: host });
    await frame();

    /* (1) The component found the drawer's seam by itself. */
    const seam = host.querySelector<HTMLElement>('[data-nc-drawer-seam]')!;
    const card = host.querySelector<HTMLElement>('[data-nc-drawer]')!;
    expect(seam).not.toBeNull();
    expect(dots()).toHaveLength(8);
    expect(seam.contains(railTrack())).toBe(true);
    expect(card.contains(railTrack())).toBe(false);

    /* (2) Entering, as one object. */
    const timing = (element: Element) => {
      const style = getComputedStyle(element);
      return `${style.animationName} ${style.animationDuration} ${style.animationTimingFunction}`;
    };
    expect(timing(seam)).toBe(timing(card));
    const enteringName = getComputedStyle(seam).animationName;
    expect(enteringName).not.toBe('none');

    /* (2, exit) and (3). Closing puts both into the leaving animation on the
       same timing, and the rail is gone once it has finished. */
    view.rerender(<Harness open={false} />);
    await frame();
    const leavingCard = host.querySelector<HTMLElement>('[data-nc-drawer]')!;
    const leavingSeam = host.querySelector<HTMLElement>('[data-nc-drawer-seam]')!;
    /* A different animation from the one it entered on, and the same one as the
       card's — the pair, so neither "it is still animating" nor "they agree on
       nothing" passes. */
    expect(getComputedStyle(leavingSeam).animationName).not.toBe(enteringName);
    expect(timing(leavingSeam)).toBe(timing(leavingCard));

    /* The exit is one `--motion-medium`; wait it out rather than guessing. */
    const startedAt = performance.now();
    while (host.querySelector('[data-nc-drawer]') !== null
      && performance.now() - startedAt < 2_000) await pause(20);

    expect(host.querySelector('[data-nc-drawer]')).toBeNull();
    expect(host.querySelector('[data-nc-drawer-seam]')).toBeNull();
    /* The dots went with it — searched from the whole document, not from the
       seam, so a rail left behind anywhere is caught. */
    expect(dots()).toHaveLength(0);
    host.remove();
  });
});

/*
 * ── The reply keeps the report's voice after the words became markdown ─────
 *
 * `.reply` used to set `font-family` on the one element that held the text, so
 * the family and the words were the same box. They are not any more: Astryx's
 * `Markdown` emits its own blocks, and those take their family, size and
 * leading from *its* variables (`--font-family-body`, `--text-body-size`,
 * `--text-body-leading`), which `styles/astryx-theme.css` maps app-wide to our
 * **sans** at the interface rank. `.reply` overrides the three for its subtree.
 *
 * That override is invisible to every other tier: jsdom computes no styles, and
 * reading the declaration out of the stylesheet would only prove it was
 * written — the failure this guards is that it stops *connecting*, which is
 * what an upstream rename of any of the three variable names would do, silently
 * and with every existing assertion still green.
 *
 * Compared against probes carrying the page's own tokens rather than against a
 * font-name string, for the same reason the composer's popover case does it:
 * what is pinned is that the hook connects, not how a family is spelled.
 */
describe('the reply’s type, through Astryx’s markdown', () => {
  const MARKDOWN_REPLY: ConversationTurn[] = [
    { id: 'you-0', author: 'you', text: 'Ask', atMs: 0 },
    {
      id: 'agent-0',
      author: 'agent',
      text: '## A heading\n\nAn answer that runs long enough to wrap.',
      atMs: 1,
    },
  ];

  /**
   * The block Astryx actually painted the words into — **and never `.reply`
   * itself**, which is only the box around it.
   *
   * There was a `?? reply` fallback here and it made the whole case vacuous:
   * with it, the pre-markdown implementation — `<p className={styles.reply}>` —
   * passes every assertion below, because `.reply` carries the plain
   * `font-family`/`font-size` declarations that are still in the rule for the
   * container's own text nodes. What is under test is that the *variables*
   * reach Astryx's block, so the block has to be found or the case has to fail.
   */
  /* Two functions rather than one taking a selector: `architecture/
     no-class-dom-query` requires every runtime query to be a static string, and
     a test file is not exempt from a rule whose point is that a selector built
     at runtime fails closed. */
  function paintedParagraph(): HTMLElement {
    const found = replies()[0].querySelector<HTMLElement>('[role="paragraph"], p');
    expect(found, 'no paragraph inside the reply — markdown did not render').not.toBeNull();
    return found!;
  }

  function paintedHeading(): HTMLElement {
    const found = replies()[0].querySelector<HTMLElement>('h4');
    expect(found, 'no h4 inside the reply — markdown did not render the heading').not.toBeNull();
    return found!;
  }

  function probe(styles: Partial<CSSStyleDeclaration>): {
    fontFamily: string; fontSize: string; lineHeight: string;
  } {
    const element = document.createElement('div');
    Object.assign(element.style, styles);
    document.body.append(element);
    const computed = getComputedStyle(element);
    /* Read every property before the element leaves the document. */
    const snapshot = {
      fontFamily: computed.fontFamily,
      fontSize: computed.fontSize,
      lineHeight: computed.lineHeight,
    };
    element.remove();
    return snapshot;
  }

  it('paints the reply in the report’s serif at the drawer’s step, not Astryx’s body sans', () => {
    render(<RailPane turns={MARKDOWN_REPLY} />);
    const painted = getComputedStyle(paintedParagraph());

    /* All three overridden variables, because all three are separately
       droppable: an earlier version of this case read family and size only, and
       deleting `--text-body-leading` left it green. */
    const wanted = probe({
      fontFamily: 'var(--font-serif)',
      fontSize: 'var(--text-md)',
      lineHeight: 'var(--leading-loose)',
    });
    expect(painted.fontFamily).toBe(wanted.fontFamily);
    expect(painted.fontSize).toBe(wanted.fontSize);
    expect(painted.lineHeight).toBe(wanted.lineHeight);

    /* And the sans the app-wide bridge would otherwise have handed it is a
       different answer, so the line above cannot pass by falling through. */
    expect(painted.fontFamily).not.toBe(probe({ fontFamily: 'var(--font-sans)' }).fontFamily);
  });

  /*
   * Headings take a *different* Astryx variable (`--font-family-heading`), which
   * the app-wide bridge maps to `--font-display` — the sans. `base.css` sets
   * `.calm-prose h1/h2/h3` in `--font-serif`, so a reply whose `##` came out
   * sans is the split voice `.reply` exists to close, one element deeper. This
   * is its own case because the body override cannot fail it and did not.
   */
  it('paints the reply’s own headings in the same serif, not the display sans', () => {
    render(<RailPane turns={MARKDOWN_REPLY} />);
    const heading = getComputedStyle(paintedHeading());

    expect(heading.fontFamily).toBe(probe({ fontFamily: 'var(--font-serif)' }).fontFamily);
    expect(heading.fontFamily).not.toBe(probe({ fontFamily: 'var(--font-display)' }).fontFamily);
  });
});

/*
 * ── How many rows an activity line costs, which only an engine can say ─────
 *
 * `.activityDetail` needs a row of its own, and two attempts to get one out of
 * `flex-wrap` both misfired. A flex line **fills and wraps before it shrinks**,
 * and `.activityTarget` is `overflow: hidden`, which zeroes its automatic
 * minimum size — so under `nowrap` a 64-character command shrinks and
 * ellipsizes beside `Ran`, and under `wrap` the same command (11px mono, ~420px,
 * in a 364px column) takes a row of its own and shoves `Failed` and the duration
 * onto another. Unconditional wrap turned every long `done` line into two;
 * confining it to lines with a detail turned every failed line into *four*.
 * The structure carries it now: a `nowrap` `.activityRow` plus the detail block.
 *
 * jsdom computes no layout, so every one of these variants produces identical
 * DOM and identical `textContent` there; `public.test.tsx` cannot see this and
 * could not be made to. It is a claim about rows on a page, which means it is a
 * claim only a rendering engine can be asked about, and this is the file that
 * asks.
 *
 * The pair brackets the behaviour rather than pinning one side of it, and both
 * cases count rows rather than comparing heights to a threshold — "the failed
 * line is taller than one row" is the assertion the four-row layout walked
 * straight through. A test that only said "the failed line takes two rows" is
 * still green under a stylesheet that never wraps at all and loses the detail
 * row; a test that only said "the done line takes one" is green under the same.
 * Both together admit exactly one implementation.
 */
describe('the activity line’s row count, as the engine lays it out', () => {
  /** A real command as the domain hands it over: `clip()` cuts at
   *  `ACTIVITY_TARGET_MAX` and marks the cut, so 64 characters is exactly the
   *  widest noun that can reach this component — the worst case, and a common
   *  one, since every `cargo`/`npm` invocation with flags is longer than that. */
  const LONG_TARGET = 'cargo clippy --workspace --all-targets --all-features -- -D war…';

  function activity(overrides: Partial<ConversationActivity>): ConversationActivity {
    return {
      id: 'a1', author: 'activity', verb: 'Ran', target: LONG_TARGET, state: 'done',
      durationMs: null, detail: null, atMs: 0, ...overrides,
    };
  }

  /** The activity paragraphs on the page, in order. `[data-nc-state]` is the
   *  component's own hook and a static selector; the spans inside it carry only
   *  hashed module classes, so they are reached positionally. */
  const lines = () => [...document.querySelectorAll<HTMLElement>('p[data-nc-state]')];

  /* The first row is the line's first child; the verb and the noun are the
     first two children of *it*. Positional because the spans below the
     paragraph carry only hashed module classes, which
     `architecture/no-class-dom-query` forbids reaching for. */
  const rowOf = (line: HTMLElement) => line.children[0] as HTMLElement;
  const verbOf = (line: HTMLElement) => rowOf(line).children[0] as HTMLElement;
  const nounOf = (line: HTMLElement) => rowOf(line).children[1] as HTMLElement;

  /** Two boxes are on the same row when their vertical extents overlap — not
   *  when their tops are equal. These spans are set in different families and
   *  aligned on their *baselines*, so on one row their box tops legitimately
   *  differ by about a pixel. What cannot happen on one row is one box starting
   *  at or below where the other ends. */
  const sameRow = (a: HTMLElement, b: HTMLElement) => {
    const [x, y] = [a.getBoundingClientRect(), b.getBoundingClientRect()];
    return x.top < y.bottom && y.top < x.bottom;
  };

  it('keeps a long done line on one row, ellipsized beside the verb', async () => {
    await page.viewport(1400, 900);
    expect(LONG_TARGET).toHaveLength(64);
    render(<RailPane turns={[activity({}), activity({ id: 'a2', target: 'ls' })]} />);
    await frame();

    const [long, short] = lines();
    /* Same height as a line whose noun is two characters: the long one did not
       gain a row. Compared against a rendered sibling rather than a literal,
       so the case survives a change to the caption's leading. */
    expect(long.getBoundingClientRect().height)
      .toBe(short.getBoundingClientRect().height);
    /* And the noun is on the verb's own row — the shape `nowrap` produces. */
    expect(sameRow(nounOf(long), verbOf(long))).toBe(true);
    /* It is ellipsized rather than merely fitting, which is the other half of
       "shrink, don't wrap": the box is narrower than the text inside it. */
    expect(nounOf(long).clientWidth).toBeLessThan(nounOf(long).scrollWidth);
  });

  /* The worst failed line the component can be handed: the 64-character noun,
     plus both of the things gated to be rare — a duration over the floor, and a
     reason. Everything that competes for the first row is present at once,
     which is the only configuration under which the row count can go wrong. */
  it('lays a failed line out as exactly two rows, whatever else is on it', async () => {
    await page.viewport(1400, 900);
    render(<RailPane turns={[
      activity({ state: 'failed', durationMs: 8_400, detail: 'error: no test specified' }),
      activity({ id: 'a2', target: 'ls' }),
    ]}
    />);
    await frame();

    const [failed, done] = lines();
    const row = rowOf(failed);
    /* Verb, noun, `Failed`, duration — and nothing else, so the four checked
       below are the whole of the first row rather than four of a longer list. */
    const items = [...row.children] as HTMLElement[];
    expect(items.map((item) => item.textContent))
      .toEqual(['Ran', LONG_TARGET, 'Failed', '8.4s']);

    /* (1) All four share one row. Asserted against the verb pairwise: overlap
       is not transitive, so "each overlaps the first" is the claim that
       actually rules out any of them having dropped. */
    for (const item of items.slice(1)) expect(sameRow(item, verbOf(failed))).toBe(true);

    /* (2) The reason is a row *below* all four — below the row box itself, so
       nothing on it can be beside the reason. */
    const detail = failed.children[1] as HTMLElement;
    expect(detail.textContent).toBe('error: no test specified');
    expect(detail.getBoundingClientRect().top)
      .toBeGreaterThanOrEqual(row.getBoundingClientRect().bottom);

    /* (3) And the paragraph is those two rows and no more. Written as an
       equality against the two children's own heights rather than as "taller
       than one row": the four-row layout this case exists for is taller than
       one row too, and that is precisely how it survived the last round. The
       first row's height is checked against a plain `done` line so that (1)'s
       overlaps cannot be satisfied by a row that has itself grown. */
    const box = failed.getBoundingClientRect();
    expect(box.height).toBeCloseTo(
      row.getBoundingClientRect().height + detail.getBoundingClientRect().height, 1,
    );
    expect(row.getBoundingClientRect().height)
      .toBeCloseTo(done.getBoundingClientRect().height, 1);

    /* (4) And the noun is still ellipsized rather than fitting, which is the
       property `nowrap` was protecting and the property a wrapping line loses:
       on a failed line, with `Failed` and `8.4s` also on the row, the command
       has less room than on a `done` line, not more. */
    expect(nounOf(failed).clientWidth).toBeLessThan(nounOf(failed).scrollWidth);
  });
});

/**
 * WCAG 2.x relative luminance from whatever `getComputedStyle` hands back.
 *
 * Chromium serialises these tokens as `oklch(L C H)` rather than converting to
 * `rgb()`, which is the one detail that makes this function longer than a line:
 * the conversion to linear-light sRGB has to happen here. Written out rather
 * than imported because the only other copy in the repository
 * (`tools/styles/check-contrast.mjs`) reads token *declarations* off the
 * stylesheet; this one has to read what the engine actually painted, which is
 * the whole reason the pair could not simply be added there.
 */
function relativeLuminance(color: string): number {
  const numbers = [...color.matchAll(/-?[\d.]+/g)].map((match) => Number(match[0]));
  const linear = color.startsWith('oklch') ? oklchToLinear(color, numbers) : numbers.slice(0, 3)
    .map((value) => {
      const channel = value / 255;
      return channel <= 0.040_45 ? channel / 12.92 : ((channel + 0.055) / 1.055) ** 2.4;
    });
  return 0.2126 * linear[0] + 0.7152 * linear[1] + 0.0722 * linear[2];
}

function oklchToLinear(color: string, [lightness, chroma, hue]: number[]): number[] {
  const L = color.includes('%') ? lightness / 100 : lightness;
  const radians = hue * Math.PI / 180;
  const a = chroma * Math.cos(radians);
  const b = chroma * Math.sin(radians);
  const l = (L + 0.396_337_777_4 * a + 0.215_803_757_3 * b) ** 3;
  const m = (L - 0.105_561_345_8 * a - 0.063_854_172_8 * b) ** 3;
  const s = (L - 0.089_484_177_5 * a - 1.291_485_548 * b) ** 3;
  return [
    4.076_741_662_1 * l - 3.307_711_591_3 * m + 0.230_969_929_2 * s,
    -1.268_438_004_6 * l + 2.609_757_401_1 * m - 0.341_319_396_5 * s,
    -0.004_196_086_3 * l - 0.703_418_614_7 * m + 1.707_614_701 * s,
  ];
}

function contrast(a: string, b: string): number {
  const [hi, lo] = [relativeLuminance(a), relativeLuminance(b)].sort((x, y) => y - x);
  return (hi + 0.05) / (lo + 0.05);
}
