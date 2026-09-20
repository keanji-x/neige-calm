/* The composer, the exchange rail and the reply's type, measured against a real rendering engine. */
import { act, render } from '@testing-library/react';
import { page, userEvent } from 'vitest/browser';
import { afterEach, describe, expect, it, vi } from 'vitest';

/* The whole cascade before the component: `thread.module.css` declares `@layer features` of its own, and whichever registers first wins. */
import '../../../styles/entry.css';

import { ChatComposer, ChatThread } from './public.tsx';
import type {
  Conversation, ConversationActivity, ConversationSystemEntry, ConversationTurn,
  ConversationTurnOutcome, OptimisticConversationTurn, TranscriptEntry,
} from '../../../../../core/domain/conversation.ts';
import { Drawer } from '../../../ui/drawer/public.tsx';
import drawerStyles from '../../../ui/drawer/drawer.module.css';
import { useState } from '../../../ui/state/public.ts';

afterEach(() => { document.body.replaceChildren(); });

/** The cascade order the document ended up with, read off the first top-level `@layer` rule in sheet order (registration is first-come). It stops at the first top-level statement, so `@import layer()` and conditional layers are invisible to it. */
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

/** A card with an image picked and nothing typed, which is the one state that
 *  renders the composer's own send door beside Astryx's. */
function ImageOnlyCard() {
  return (
    <div style={{ position: 'absolute', insetBlock: 20, insetInlineEnd: 24, inlineSize: 396 }}>
      <div
        style={{
          ['--nc-card-inset' as string]: '8px',
          ['--nc-card-radius' as string]: '16px',
        }}
      >
        <ChatComposer onSend={vi.fn()} allowEmptyText onNewConversation={vi.fn()} />
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
  it('registers the cascade in the production order', () => {
    expect(registeredLayerOrder()).toEqual(PRODUCTION_LAYER_ORDER);
  });

  it('spans exactly the input well, one card inset inside the composer box', async () => {
    await openMenu();
    const box = composer().getBoundingClientRect();
    const lid = popover().getBoundingClientRect();
    const inset = 8;
    expect(lid.width).toBeGreaterThan(100);
    expect(Math.round(lid.left - box.left)).toBe(inset);
    expect(Math.round(box.right - lid.right)).toBe(inset);
    expect(Math.round(lid.bottom)).toBeLessThanOrEqual(Math.round(box.top + inset) + 1);
  });

  /* `--color-background-popover` and `--shadow-low` are Astryx's internal names; Astryx paints them on the popover's child, not the `[popover]` box. */
  it('paints the menu on --paper with the float shadow, not on a fallback surface', async () => {
    await openMenu();
    /* `[popover]` itself is transparent; Astryx paints the box inside it. */
    const surface = popover().firstElementChild as HTMLElement;
    const painted = getComputedStyle(surface);

    const probe = document.createElement('div');
    probe.style.backgroundColor = 'var(--paper)';
    probe.style.boxShadow = 'var(--shadow-float)';
    document.body.append(probe);
    const wanted = getComputedStyle(probe);

    expect(painted.backgroundColor).toBe(wanted.backgroundColor);
    expect(painted.boxShadow).toBe(wanted.boxShadow);

    const wellFill = getComputedStyle(composer()).getPropertyValue('--bg').trim();
    probe.style.backgroundColor = wellFill;
    expect(painted.backgroundColor).not.toBe(getComputedStyle(probe).backgroundColor);
    probe.remove();
  });

  /* The Send override is keyed on Astryx's hardcoded English `aria-label='Send'`. */
  it('fills Send with the chip surface rather than Astryx\'s accent', async () => {
    await page.viewport(1400, 900);
    render(<Card />);
    await typeInto(field(), 'Ship it');
    const send = document.querySelector<HTMLElement>('button[aria-label="Send"]')!;

    const probe = document.createElement('div');
    probe.style.backgroundColor = 'var(--surface-chip)';
    document.body.append(probe);
    expect(getComputedStyle(send).backgroundColor).toBe(getComputedStyle(probe).backgroundColor);
    probe.remove();
  });

  it('paints the composer\'s own send door in Send\'s material and on its baseline', async () => {
    await page.viewport(1400, 900);
    render(<ImageOnlyCard />);

    const door = document.querySelector<HTMLElement>('[data-nc-send-attachment]')!;
    const send = document.querySelector<HTMLElement>('button[aria-label="Send"]')!;
    const painted = getComputedStyle(door);

    const probe = document.createElement('div');
    probe.style.backgroundColor = 'var(--surface-chip)';
    document.body.append(probe);
    expect(painted.backgroundColor).toBe(getComputedStyle(probe).backgroundColor);
    probe.style.backgroundColor = getComputedStyle(composer()).getPropertyValue('--bg').trim();
    expect(painted.backgroundColor).not.toBe(getComputedStyle(probe).backgroundColor);
    probe.remove();

    const doorBox = door.getBoundingClientRect();
    const sendBox = send.getBoundingClientRect();
    expect(doorBox.height).toBe(sendBox.height);
    expect(Math.abs(doorBox.top - sendBox.top)).toBeLessThan(1);
    expect(doorBox.right).toBeLessThanOrEqual(sendBox.left + 1);
  });

  /* `check-contrast.mjs` does not read this stylesheet, so the 4.5:1 this text needs has no gate of its own. */
  it('right-edges the queued caption onto its message and keeps it legible', async () => {
    await page.viewport(1400, 900);
    const queuedTurn: OptimisticConversationTurn = {
      id: 'echo-1', author: 'you', text: 'A message that is waiting its turn.',
      atMs: 4_000, serverHighWaterBefore: 0, queued: true, entryId: null,
    };
    render(<RailPane turns={[...railTurns(1), queuedTurn]} />);

    const note = document.querySelector<HTMLElement>('[data-nc-queued-note]')!;
    const said = document.querySelector<HTMLElement>('[data-nc-queued]')!;
    const painted = getComputedStyle(note);
    expect(painted.textAlign).toBe('end');
    expect(Math.abs(note.getBoundingClientRect().right - said.getBoundingClientRect().right))
      .toBeLessThan(1);

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

/* Both router call sites pass a `disabled` that goes true inside the very click that sends; Chromium hands focus to `<body>` when the editable holding it stops being editable, and jsdom does not model that. */
const flightsInProgress: Array<() => void> = [];

/** The composer as the router builds it: `disabled` goes true inside the click that sends and clears when the request lands; `withStop` keeps Stop live past that window, as the router does. */
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

/** Send with a real Enter: `:focus-visible` is decided from the engine's own record of the last interaction, which a synthetic `KeyboardEvent` does not write. */
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

    expect(document.querySelector('[contenteditable="true"]')).toBeNull();
    expect(document.activeElement).toBe(composer());

    await land();

    expect(document.activeElement).toBe(field());
  });

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

  /* The `:focus-visible` match is asserted first: without it the outline reading passes because the pseudo-class never engaged. */
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
    expect(perch.getAttribute('aria-label')).not.toBe('Message');

    expect(perch.matches(':focus-visible')).toBe(true);
    expect(getComputedStyle(perch).outlineStyle).toBe('none');

    await land();
  });

  it('leaves focus on Stop when the reader aimed at it during the flight', async () => {
    await page.viewport(1400, 900);
    render(<Sending withStop />);
    await typeInto(field(), 'Ship it');
    const send = document.querySelector<HTMLElement>('button[aria-label="Send"]')!;
    await act(async () => { send.click(); await Promise.resolve(); });

    const stop = document.querySelector<HTMLElement>('button[aria-label="Stop"]')!;
    expect(composer().contains(stop)).toBe(true);
    stop.focus();
    expect(document.activeElement).toBe(stop);

    await land();

    expect(document.querySelector('button[aria-label="Stop"]')).toBe(stop);
    expect(document.activeElement).toBe(stop);
    expect(document.activeElement).not.toBe(field());
  });

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

/* jsdom resolves `[contenteditable="true"]` immediately, so only this tier can tell the caret reaching the field from the caret parked on the perch. */
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

  /* The composer is the drawer's `footer`, and the drawer's own open effect runs after it. */
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

function railConversation(): Conversation {
  return {
    id: 'c1', trackId: 'w1', trackTitle: 'Ship the rewrite', title: null, kind: 'codex',
    state: 'idle', updatedAt: 0, turns: 0,
  };
}

/** A line of reply, repeated: a marker only leaves the pane's top edge if the exchange under it is taller than the pane. */
const LINE = 'The reply runs on for a few lines so the pane has something to scroll. ';

/** `count` exchanges, `longReplies` of them answered at length; the rest answer with one word. */
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

/** A prompt between `RAIL_LABEL_MAX` and `RAIL_PREVIEW_MAX`: the accessible name is truncated and the floating preview is not. */
const LONG_PROMPT = 'Rewrite the transcript so the reply keeps the report’s voice '
  + 'and the drawer keeps its measure, and say what it costs.';

/** A prompt comfortably longer than `RAIL_PREVIEW_MAX`. */
const OVERLONG_PROMPT = `${LONG_PROMPT} ${LINE.repeat(4)}`.replace(/\s+/g, ' ').trim();

/** `count` exchanges whose questions differ only by ordinal. */
function promptTurns(count = 8, promptAt: (index: number) => string = () => LONG_PROMPT):
ConversationTurn[] {
  return Array.from({ length: count }).flatMap((_unused, index) => [
    { id: `you-${index}`, author: 'you' as const, text: promptAt(index), atMs: index * 2_000 },
    { id: `agent-${index}`, author: 'agent' as const, text: 'Short.', atMs: index * 2_000 + 1 },
  ]);
}

/** The drawer's own block insets (`--space-9` + `--space-11`): how much taller than its pane the host has to be for the card to come out at exactly `paneHeight`. */
const DRAWER_BLOCK_INSETS = 20 + 28;

/** The drawer as a four-box fixture: host, card, pane and seam. Card and seam carry the real `drawerStyles` rules (the `.drawer` clip is what the portal exists for); the animation is off and the pane takes a fixed `blockSize`. */
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
            <ChatThread cards={{}} stalled={false} conversation={railConversation()} turns={turns} />
          </div>
        </div>
      </div>
      <div className={drawerStyles.seam} data-nc-drawer-seam="" style={{ animation: 'none' }} />
    </div>
  );
}

/** The drawer's own top clearance, read back off the engine. */
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
    /* A task receipt, not a report edit: a `Report edited` entry opens a quiet-sync fold. */
    const system: ConversationSystemEntry = {
      id: 'system-1', author: 'system', label: 'Task completed',
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

/* Only this tier can prove the error sentence wraps instead of riding Astryx's `nowrap` label span off the column. */
describe('a failed turn in a real engine', () => {
  it('shows Failed with the message wrapped below it, and nothing for a completed turn', async () => {
    const you: ConversationTurn = { id: 'you-1', author: 'you', text: 'Summarise everything.', atMs: 0 };
    const failed: ConversationTurnOutcome = {
      id: 'outcome-1', author: 'turn', turnId: 'turn-1', status: 'failed',
      message: 'The conversation exceeded the model\'s context window and the request was rejected before any output was produced.',
      code: 'contextWindowExceeded', atMs: 0,
    };
    const completed: ConversationTurnOutcome = {
      id: 'outcome-2', author: 'turn', turnId: 'turn-2', status: 'completed', atMs: 0,
    };
    render(<RailPane turns={[you, failed, { ...you, id: 'you-2' }, completed]} />);
    await frame();

    const outcomes = document.querySelectorAll<HTMLElement>('[data-nc-turn="outcome"]');
    expect(outcomes).toHaveLength(1);
    const outcome = outcomes[0];
    expect(outcome.dataset['ncTurnOutcome']).toBe('failed');
    expect(outcome.querySelector('[role="status"]')?.textContent).toBe('Failed');
    const message = outcome.querySelector<HTMLElement>('[data-nc-turn-outcome-message]')!;
    expect(message.textContent).toBe(failed.message);
    const label = outcome.querySelector<HTMLElement>('[role="status"]')!;
    const labelBox = label.getBoundingClientRect();
    const messageBox = message.getBoundingClientRect();
    expect(messageBox.top).toBeGreaterThanOrEqual(labelBox.bottom);
    expect(messageBox.width).toBeLessThanOrEqual(labelBox.width + 1);
    expect(messageBox.height).toBeGreaterThan(Number.parseFloat(getComputedStyle(message).lineHeight) * 1.5);
    expect(outcome.querySelector('[data-nc-turn-outcome-hint]')?.textContent)
      .toBe('The conversation no longer fits in the model’s context window.');
  });
});

/** Two frames: one for the scroll handler's own rAF, one for the render it
 *  schedules. */
async function settle() {
  await frame();
  await frame();
}

/** Put the pane where a reader would have put it, by the same `scrollTop` write the rail's press makes. */
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

/** Put the pointer at a client-Y inside the rail and let the envelope settle: the wait outlasts the `--motion-instant` transition on the dot's size. */
async function pointRailAt(clientY: number) {
  railTrack().dispatchEvent(new PointerEvent('pointermove', {
    bubbles: true, pointerType: 'mouse', clientY,
  }));
  await settle();
  await pause(150);
}

/** One rule written against one of `element`'s own classes, with its media conditions, pseudo-element and document order. */
type RailRule = Readonly<{
  rule: CSSStyleRule;
  at: number;
  conditions: readonly string[];
  pseudo: string;
}>;

/** Every style rule in the document written against one of `element`'s own classes, in document order. `at` counts every rule the walk passes: `@media (prefers-reduced-motion)` wins over `(pointer: fine)` only by coming later. */
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
  it('spends nothing on the transcript, and lives in the drawer’s seam', async () => {
    await page.viewport(1400, 900);
    render(<RailPane turns={railTurns(2)} />);
    await frame();
    expect(dots()).toHaveLength(2);
    const bare = replies()[0].getBoundingClientRect().left;
    expect(Math.round(bare)).toBe(Math.round(pane().getBoundingClientRect().left + 8));

    document.body.replaceChildren();
    render(<RailPane turns={railTurns(8)} />);
    await frame();
    expect(dots()).toHaveLength(8);

    for (const paragraph of replies()) {
      expect(Math.round(paragraph.getBoundingClientRect().left)).toBe(Math.round(bare));
    }
    const frameBox = document.querySelector<HTMLElement>('[data-nc-thread]')!.parentElement!;
    expect(getComputedStyle(frameBox).display).not.toBe('grid');

    const seam = document.querySelector<HTMLElement>('[data-nc-drawer-seam]')!;
    const rail = railTrack().getBoundingClientRect();
    expect(Math.round(seam.getBoundingClientRect().width)).toBe(24);
    expect(Math.round(rail.width)).toBe(24);

    const pitch = Number.parseFloat(
      getComputedStyle(railTrack()).getPropertyValue('--nc-rail-pitch'),
    );
    expect(pitch).toBe(12);
    const dot = dots()[0].getBoundingClientRect();
    expect(Math.round(dot.height)).toBe(pitch);
    expect(Math.round(dot.width)).toBe(24);
    const centres = dots().map((each) => {
      const box = each.getBoundingClientRect();
      return box.top + box.height / 2;
    });
    for (let index = 1; index < centres.length; index += 1) {
      expect(centres[index] - centres[index - 1]).toBeCloseTo(12, 1);
    }

    const card = document.querySelector<HTMLElement>('[data-nc-drawer]')!;
    for (const each of dots()) {
      expect(each.getBoundingClientRect().left)
        .toBeGreaterThanOrEqual(card.getBoundingClientRect().right);
    }
  });

  /* `.drawer` is `overflow: hidden`; asserted through `checkVisibility()` because a clipped element still reports a rect. */
  it('paints the rail outside the card’s clip', async () => {
    await page.viewport(1400, 900);
    render(<RailPane turns={railTurns(8)} />);
    await frame();
    const card = document.querySelector<HTMLElement>('[data-nc-drawer]')!;
    expect(getComputedStyle(card).overflow).toBe('hidden');
    expect(card.contains(railTrack())).toBe(false);
    expect(railTrack().checkVisibility()).toBe(true);

    const probe = document.createElement('div');
    probe.style.cssText = 'position:absolute;inset-block-start:0;'
      + 'inset-inline-start:100%;inline-size:24px;block-size:24px;background:red';
    document.querySelector<HTMLElement>('[data-nc-rail-pane-inner]')!.append(probe);
    const clipped = probe.getBoundingClientRect();
    expect(clipped.left).toBeGreaterThanOrEqual(card.getBoundingClientRect().right);
    probe.remove();
  });

  it('holds the rail still while the transcript scrolls under it', async () => {
    await page.viewport(1400, 900);
    render(<RailPane turns={railTurns(8)} />);
    await frame();
    await scrollPaneTo(0);
    const at0 = railTrack().getBoundingClientRect();
    const seam = document.querySelector<HTMLElement>('[data-nc-drawer-seam]')!;
    const rail = railTrack().parentElement!.getBoundingClientRect();
    expect(rail.top).toBeCloseTo(seam.getBoundingClientRect().top, 0);
    expect(rail.top).not.toBeCloseTo(pane().getBoundingClientRect().top + paneInsetTop(), 0);

    await scrollPaneTo(400);
    expect(replies()[0].getBoundingClientRect().top)
      .toBeLessThan(pane().getBoundingClientRect().top);
    expect(railTrack().getBoundingClientRect().top).toBeCloseTo(at0.top, 0);

    await scrollPaneTo(900);
    expect(railTrack().getBoundingClientRect().top).toBeCloseTo(at0.top, 0);
    expect(getComputedStyle(railTrack().parentElement!).position).toBe('relative');
  });

  /* Capped by `--nc-rail-max`: a thin column of ink running the full height of the page's pad reads as a scrollbar and gets grabbed. */
  it('caps the track at a fixed length and centres it in the seam', async () => {
    await page.viewport(1400, 900);
    render(<RailPane turns={railTurns(40, 0)} paneHeight={400} />);
    await frame();
    const seam = document.querySelector<HTMLElement>('[data-nc-drawer-seam]')!.getBoundingClientRect();
    const track = railTrack().getBoundingClientRect();
    expect(Math.round(seam.height)).toBe(400);
    const cap = Number.parseFloat(
      getComputedStyle(railTrack()).getPropertyValue('--nc-rail-max'),
    );
    expect(cap).toBe(320);
    expect(Math.round(track.height)).toBe(320);
    expect(track.bottom).toBeLessThanOrEqual(seam.bottom + 0.5);
    expect(track.top - seam.top).toBeCloseTo(seam.bottom - track.bottom, 0);
    expect(track.top - seam.top).toBeGreaterThan(1);
    expect(railTrack().scrollHeight).toBeGreaterThan(railTrack().clientHeight);
    const frameBox = document.querySelector<HTMLElement>('[data-nc-thread]')!.parentElement!;
    expect(frameBox.style.getPropertyValue('--nc-rail-room')).toBe('');
    expect(frameBox.style.getPropertyValue('--nc-rail-reach')).toBe('');
  });

  it('centres a column that fits in the seam’s own block centre', async () => {
    await page.viewport(1400, 900);
    render(<RailPane turns={railTurns(8)} paneHeight={400} />);
    await frame();
    const track = railTrack();
    expect(dots()).toHaveLength(8);
    expect(track.scrollHeight).toBeLessThanOrEqual(track.clientHeight + 1);

    const seam = document.querySelector<HTMLElement>('[data-nc-drawer-seam]')!
      .getBoundingClientRect();
    const first = dots()[0].getBoundingClientRect();
    const last = dots()[7].getBoundingClientRect();
    expect((first.top + last.bottom) / 2).toBeCloseTo((seam.top + seam.bottom) / 2, 0);
    expect(first.top).toBeGreaterThan(seam.top + 1);
    expect(last.bottom).toBeLessThan(seam.bottom - 1);
  });

  /* Plain `justify-content: center` on a scrollport puts half the overflow before the scroll origin, where no gesture can reach; `safe center` degrades to `start`. */
  it('keeps the first and the last dot reachable when the column overflows', async () => {
    await page.viewport(1400, 900);
    render(<RailPane turns={railTurns(40, 0)} paneHeight={400} />);
    await frame();
    const track = railTrack();
    expect(dots()).toHaveLength(40);
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

  /* The box that clips the ring is `.railTrack` (`overflow-y: auto` makes `overflow-x` auto too), not the pane; the ring only exists under `:focus-visible`. */
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

    /* The offset pushes the outline edge outward and the stroke is painted outward from there. */
    const reach = Number.parseFloat(ring.outlineOffset) + width;
    const dot = dots()[0].getBoundingClientRect();
    const track = railTrack().getBoundingClientRect();
    expect(dot.left - reach).toBeGreaterThanOrEqual(track.left - 0.01);
    expect(dot.right + reach).toBeLessThanOrEqual(track.right + 0.01);
  });

  /* The dot is the control, so 3:1 against the surface behind it is the requirement, measured off the rendered pseudo-element. The surface is the page (`--bg`), not the pane. */
  it('paints the resting dot between 3:1 and the text ladder, against the page', async () => {
    await page.viewport(1400, 900);
    render(<RailPane turns={railTurns(8)} />);
    await frame();
    const seam = document.querySelector<HTMLElement>('[data-nc-drawer-seam]')!;
    const ink = getComputedStyle(dots()[0], '::before').backgroundColor;
    const surface = getComputedStyle(document.body).backgroundColor;
    expect(getComputedStyle(seam).backgroundColor).toBe('rgba(0, 0, 0, 0)');
    expect(surface).not.toBe(getComputedStyle(pane()).backgroundColor);

    const ratio = contrast(ink, surface);
    expect(ratio).toBeGreaterThanOrEqual(3);
    /* The 3:1 line is between `L62%` and `L64%` in this theme; 3.2 is clear of it. */
    expect(ratio).toBeGreaterThan(3.2);
    expect(ratio).toBeLessThan(5);

    const token = (name: string) =>
      getComputedStyle(document.documentElement).getPropertyValue(name).trim();
    expect(contrast(token('--text-4'), surface)).toBeLessThan(3);
    expect(contrast(token('--text-3'), surface)).toBeGreaterThan(5);

    expect(contrast(token('--text-2'), surface)).toBeGreaterThanOrEqual(3);
    expect(contrast(token('--accent'), surface)).toBeGreaterThanOrEqual(3);
  });

  /* The falloff is a smoothstep: at one dot out it is above a straight ramp (0.844 vs 0.750) and at three below (0.156 vs 0.250); at two they agree exactly, so the discriminating pair is 1 and 3. */
  it('swells the dots around the pointer and settles back to rest', async () => {
    await page.viewport(1400, 900);
    render(<RailPane turns={railTurns(12, 2)} />);
    await frame();
    await scrollPaneTo(0);
    /* Park the real pointer off the rail: the engine fires boundary events when the element under a stationary cursor changes. */
    await userEvent.hover(pane());
    await pause(150);
    const rest = dotInk(11);
    expect(rest).toBe(4);

    const aimed = dots()[5].getBoundingClientRect();
    await pointRailAt(aimed.top + aimed.height / 2);

    const peak = Number.parseFloat(
      getComputedStyle(railTrack()).getPropertyValue('--nc-rail-dot-peak'),
    );
    expect(peak).toBe(8);
    expect(dotInk(5)).toBeCloseTo(peak, 1);

    expect(dotInk(6)).toBeGreaterThan(dotInk(7));
    expect(dotInk(7)).toBeGreaterThan(dotInk(8));
    expect(dotInk(8)).toBeGreaterThan(dotInk(9));
    expect(dotInk(6)).toBeGreaterThan(rest + 1);
    expect(dotInk(7)).toBeGreaterThan(rest + 0.5);
    expect(dotInk(9)).toBeCloseTo(rest, 1);
    expect(dotInk(6)).toBeGreaterThan(7.2);
    expect(dotInk(8)).toBeLessThan(4.8);
    expect(dotInk(4)).toBeCloseTo(dotInk(6), 1);

    railTrack().dispatchEvent(new PointerEvent('pointerleave', { pointerType: 'mouse' }));
    await settle();
    await pause(150);
    /* Dot 0 is the lit one and rests at `--nc-rail-dot-current`. */
    expect(dotInk(0)).toBeCloseTo(6, 1);
    for (let index = 1; index < 12; index += 1) expect(dotInk(index)).toBeCloseTo(rest, 1);
    for (const dot of dots()) expect(dot.style.getPropertyValue('--nc-dot-lift')).toBe('');
  });

  /* Measured between rendered target centres with a real pointer on the middle dot; ≥24 is the criterion's number, not this build's. Not a claim of conformance: at rest the targets are 12px apart. */
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
    expect(centre(6) - centre(5)).toBeCloseTo(12, 1);

    const aimed = dots()[5].getBoundingClientRect();
    await pointRailAt(aimed.top + aimed.height / 2);

    expect(centre(5) - centre(4)).toBeGreaterThanOrEqual(24);
    expect(centre(6) - centre(5)).toBeGreaterThanOrEqual(24);
    expect(dots()[5].getBoundingClientRect().height).toBeGreaterThanOrEqual(24);

    railTrack().dispatchEvent(new PointerEvent('pointerleave', { pointerType: 'mouse' }));
    await settle();
    await pause(150);
    expect(centre(6) - centre(5)).toBeCloseTo(12, 1);
  });

  /* A shoulder of blank at each end is exactly the growth missing from its side, so the column's length — and the track's `scrollHeight` — do not depend on the pointer. */
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
      expect(centres()[index]).toBeCloseTo(before[index], 0);
      const after = dots()[index].getBoundingClientRect();
      expect(y).toBeGreaterThanOrEqual(after.top);
      expect(y).toBeLessThanOrEqual(after.bottom);
      expect(railTrack().scrollHeight).toBe(extentBefore);
      railTrack().dispatchEvent(new PointerEvent('pointerleave', { pointerType: 'mouse' }));
      await settle();
      await pause(150);
    };

    render(<RailPane turns={railTurns(8, 2)} />);
    await frame();
    await scrollPaneTo(0);
    await userEvent.hover(pane());
    await pause(200);
    expect(railTrack().scrollHeight).toBeLessThanOrEqual(railTrack().clientHeight + 1);
    for (const index of [0, 3, 6, 7]) await holdsStillAt(index);

    document.body.replaceChildren();
    render(<RailPane turns={railTurns(40, 0)} paneHeight={400} />);
    await frame();
    await userEvent.hover(pane());
    await pause(200);
    railTrack().scrollTop = 100;
    await settle();
    expect(railTrack().scrollHeight).toBeGreaterThan(railTrack().clientHeight + 1);
    for (const index of [4, 12, 30]) await holdsStillAt(index);
    expect(railTrack().scrollTop).toBe(100);
  });

  it('keeps the end dots reachable while the rail is spread open', async () => {
    await page.viewport(1400, 900);
    render(<RailPane turns={railTurns(40, 0)} paneHeight={400} />);
    await frame();
    const track = railTrack();
    expect(dots()).toHaveLength(40);
    expect(track.scrollHeight).toBeGreaterThan(track.clientHeight + 1);

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

  /* The component publishes the shoulder only from a pointer event, so on an untouched rail the stylesheet's `var(…, 2)` fallback is the layout — and that number is `RAIL_SPREAD_SPAN ÷ 2`, written in the other file. */
  it('rests on a shoulder of half the spread span, before any pointer', async () => {
    await page.viewport(1400, 900);
    render(<RailPane turns={railTurns(8, 2)} paneHeight={400} />);
    await frame();
    const track = railTrack();
    const number = (name: string) =>
      Number.parseFloat(getComputedStyle(track).getPropertyValue(name));
    const opening = number('--nc-rail-pitch-open') - number('--nc-rail-pitch');
    expect(opening).toBe(16);

    expect(track.style.getPropertyValue('--nc-rail-lead')).toBe('2');
    expect(track.style.getPropertyValue('--nc-rail-tail')).toBe('2');
    expect(Number.parseFloat(getComputedStyle(dots()[0]).marginBlockStart)).toBe(2 * opening);
    expect(Number.parseFloat(getComputedStyle(dots()[7]).marginBlockEnd)).toBe(2 * opening);

    track.style.removeProperty('--nc-rail-lead');
    track.style.removeProperty('--nc-rail-tail');
    expect(Number.parseFloat(getComputedStyle(dots()[0]).marginBlockStart)).toBe(2 * opening);
    expect(Number.parseFloat(getComputedStyle(dots()[7]).marginBlockEnd)).toBe(2 * opening);
    expect(Math.round(track.getBoundingClientRect().height)).toBe(320);
    expect(getComputedStyle(track).paddingBlockStart).toBe('0px');
  });

  /* A laptop with a touchscreen matches `(pointer: fine)`, so the component's `pointerType` check is the whole guard there. */
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

    move('mouse');
    await settle();
    await pause(150);
    expect(dotInk(5)).toBeCloseTo(8, 1);

    railTrack().dispatchEvent(new PointerEvent('pointerleave', { pointerType: 'mouse' }));
    await settle();
    await pause(150);
  });

  it('re-centres the envelope when the track scrolls under the pointer', async () => {
    await page.viewport(1400, 900);
    render(<RailPane turns={promptTurns(30, () => 'Ask about the rewrite')} />);
    await frame();
    await scrollPaneTo(0);
    await userEvent.hover(pane());
    await pause(150);
    const track = railTrack();
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

    /* Which index is under the pointer is read off the boxes: while the rail is spread the rows are not all a pitch tall. */
    const under = dots().findIndex((dot) => {
      const box = dot.getBoundingClientRect();
      return at >= box.top && at <= box.bottom;
    });
    expect(under).toBeGreaterThan(8);
    /* Not `toBeCloseTo(8)`: the pointer is wherever the scroll left it relative to a row, not on a centre. */
    const inks = dots().map((_dot, index) => dotInk(index));
    expect(dotInk(under)).toBe(Math.max(...inks));
    expect(dotInk(under)).toBeGreaterThan(7.5);
    expect(dotInk(8)).toBeCloseTo(4, 1);

    railTrack().dispatchEvent(new PointerEvent('pointerleave', { pointerType: 'mouse' }));
    await settle();
    await pause(150);
  });

  /* History arrives in front of what is on screen, so the fixture prepends; appending moves nothing. */
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

    rerender(<RailPane turns={turns(0)} />);
    await settle();
    await pause(150);

    expect(dots()).toHaveLength(12);
    const under = dots().findIndex((dot) => {
      const box = dot.getBoundingClientRect();
      return at >= box.top && at <= box.bottom;
    });
    expect(under).toBeGreaterThan(5);
    const inks = dots().map((_dot, index) => dotInk(index));
    expect(dotInk(under)).toBe(Math.max(...inks));
    expect(dotInk(under)).toBeGreaterThan(7.5);
    expect(dots()[7].getAttribute('aria-label')).toContain('Ask 7');
    expect(dotInk(7)).toBeLessThan(7.5);
    expect(dotInk(7)).toBeGreaterThan(4);
    expect(dotInk(under)).toBeGreaterThan(dotInk(under - 1));
    expect(dotInk(under)).toBeGreaterThan(dotInk(under + 1));
    expect(dotInk(under - 1)).toBeGreaterThan(dotInk(under - 2));
    expect(dotInk(under + 1)).toBeGreaterThan(dotInk(under + 2));

    railTrack().dispatchEvent(new PointerEvent('pointerleave', { pointerType: 'mouse' }));
    await settle();
    await pause(150);
  });

  /* The envelope's write cache watches only the dot array's length; React keys dots by exchange id, so a same-length id swap remounts every button without inline styles while the cache still holds the old lifts. Not reachable from the product, so the fixture constructs it directly. */
  it('keeps the envelope when the ids change under the pointer', async () => {
    await page.viewport(1400, 900);
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

    expect(dots()).toHaveLength(12);
    expect(dots()[5].getAttribute('aria-label')).toContain('Ask 5');

    const inks = dots().map((_dot, index) => dotInk(index));
    expect(dotInk(5)).toBeGreaterThan(Math.max(...inks.filter((_ink, index) => index !== 5)));
    expect(dotInk(5)).toBeCloseTo(8, 1);
    expect(dotInk(6)).toBeGreaterThan(dotInk(7));
    expect(dotInk(7)).toBeGreaterThan(dotInk(8));
    expect(dotInk(8)).toBeGreaterThan(dotInk(9));
    expect(dotInk(9)).toBeCloseTo(4, 1);
    expect(centre(6) - centre(5)).toBeGreaterThanOrEqual(24);

    railTrack().dispatchEvent(new PointerEvent('pointerleave', { pointerType: 'mouse' }));
    await settle();
    await pause(150);
  });

  /* `prefers-reduced-motion` cannot be emulated on this shared page without poisoning every file after it, so the declaration and its ordinal are read instead. */
  it('drops the dot transition under reduced motion, after the fine block', async () => {
    await page.viewport(1400, 900);
    render(<RailPane turns={railTurns(8)} />);
    await frame();
    const ledger = ruleLedgerFor(dots()[1]).filter((entry) => entry.pseudo === '::before');
    const still = under(ledger, 'prefers-reduced-motion');
    const moving = under(ledger, 'pointer: fine');
    expect(still).toHaveLength(1);
    expect(moving).toHaveLength(1);

    expect(still[0].rule.style.transition).toBe('none');
    expect(moving[0].rule.style.transition).not.toBe('');
    expect(still[0].at).toBeGreaterThan(moving[0].at);
  });

  /* The delay's number is pinned on fake timers in `public.test.tsx`; a wall-clock band ran out on a shared runner. Here: not up at 150ms, up inside a discoverability ceiling. */
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
    /* A usability ceiling, not a band around the constant. */
    expect(shownAfter).toBeLessThan(1_200);

    const name = dots()[3].getAttribute('aria-label')!;
    expect(preview.textContent).toBe(LONG_PROMPT);
    expect(name.length).toBeLessThan(LONG_PROMPT.length);
    expect(name).toContain('…');

    expect(preview.getAttribute('aria-hidden')).toBe('true');
    expect(getComputedStyle(preview).pointerEvents).toBe('none');

    const box = preview.getBoundingClientRect();
    const seamBox = document.querySelector<HTMLElement>('[data-nc-drawer-seam]')!
      .getBoundingClientRect();
    const railBox = railTrack().getBoundingClientRect();
    expect(box.right).toBeLessThanOrEqual(railBox.left);
    const card = document.querySelector<HTMLElement>('[data-nc-drawer]')!.getBoundingClientRect();
    expect(box.left).toBeGreaterThanOrEqual(card.left);
    expect(pane().contains(preview)).toBe(false);
    expect(pane().scrollWidth).toBeLessThanOrEqual(pane().clientWidth);
    expect(box.top).toBeGreaterThanOrEqual(seamBox.top - 1);
    expect(box.bottom).toBeLessThanOrEqual(seamBox.bottom + 1);

    /* Back to rest first: `userEvent.hover` teleports the cursor to where the target's box is when called, and with the rail spread the seventh dot's box is displaced. */
    await userEvent.hover(replies()[0]);
    await pause(150);
    await userEvent.hover(dots()[6]);
    await pause(600);
    const capped = railPreview()!.textContent;
    expect(OVERLONG_PROMPT.length).toBeGreaterThan(240);
    expect(capped).toHaveLength(240);
    expect(capped.endsWith('…')).toBe(true);
    expect(OVERLONG_PROMPT.startsWith(capped.slice(0, -1))).toBe(true);

    await userEvent.hover(replies()[0]);
    await pause(150);
    expect(railPreview()).toBeNull();
  });

  /* The clamp only bites when the panel is taller than twice the room from the track's edge to the dot's centre, so the end dots carry the overlong prompt at a narrow width; the premise is asserted. */
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
    expect(track.scrollHeight).toBeGreaterThan(track.clientHeight + 1);

    track.scrollTop = 0;
    await userEvent.hover(dots()[0]);
    await pause(600);
    const first = railPreview()!.getBoundingClientRect();
    const top = track.getBoundingClientRect();
    expect(first).not.toBeNull();
    const firstDot = dots()[0].getBoundingClientRect();
    expect(first.height / 2).toBeGreaterThan(firstDot.top + firstDot.height / 2 - top.top);
    expect(first.top).toBeGreaterThanOrEqual(top.top - 1);
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

  /* The track is a scrollport; the panel is positioned against `.rail`, which does not scroll. */
  it('keeps the preview on its dot when the rail scrolls under it', async () => {
    await page.viewport(1400, 900);
    render(<RailPane turns={promptTurns(30, () => 'Ask about the rewrite')} />);
    await frame();
    await scrollPaneTo(0);
    const track = railTrack();
    expect(track.scrollHeight).toBeGreaterThan(track.clientHeight + 80);

    /* Armed with a synthetic `pointerover` and the real cursor parked off the rail: a real mouse on the rail would swap the panel to whichever dot scrolls under it. */
    await userEvent.hover(pane());
    await pause(600);
    expect(railPreview()).toBeNull();
    track.scrollTop = 0;
    dots()[8].dispatchEvent(new PointerEvent('pointerover', {
      bubbles: true, pointerType: 'mouse',
    }));
    await pause(600);
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

  /* A prompt that collapses to `''` arms the preview and renders nothing; the warm-up must not be spent on it. */
  it('still waits the full delay after resting on a dot with nothing to show', async () => {
    await page.viewport(1400, 900);
    render(<RailPane turns={promptTurns(8, (index) => (index === 2 ? '   ' : LONG_PROMPT))} />);
    await frame();
    await scrollPaneTo(0);
    await userEvent.hover(pane());
    await pause(600);
    expect(railPreview()).toBeNull();

    expect(dots()).toHaveLength(8);
    expect(dots()[2].getAttribute('aria-label')).toBe('Jump to exchange 3');

    await userEvent.hover(dots()[2]);
    await pause(600);
    expect(railPreview()).toBeNull();

    await userEvent.hover(dots()[4]);
    await pause(150);
    expect(railPreview()).toBeNull();
    await pause(450);
    expect(railPreview()!.textContent).toBe(LONG_PROMPT);

    await userEvent.hover(dots()[5]);
    await pause(120);
    expect(railPreview()).not.toBeNull();

    await userEvent.hover(replies()[0]);
    await pause(150);
  });

  /* A touchscreen's first pointer event on a control is the press; a laptop with a touchscreen reports `pointer: fine`, so the component's own check is the guard. */
  it('does not float the prompt out for a touch pointer', async () => {
    await page.viewport(1400, 900);
    render(<RailPane turns={promptTurns()} />);
    await frame();
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

  it('lights the first dot at the top of the transcript', async () => {
    await page.viewport(1400, 900);
    render(<RailPane turns={railTurns(8)} />);
    await frame();
    await scrollPaneTo(0);
    expect(currentDot()).toBe(0);
  });

  /* A press on a transcript that fits writes a `scrollTop` the engine clamps; nothing moves, so no `scroll` fires and only the re-read can correct the mark. */
  it('lights the first dot on a transcript that fits, and a press does not overrule it', async () => {
    await page.viewport(1400, 900);
    render(<RailPane turns={railTurns(5, 0)} paneHeight={800} />);
    await frame();
    const scroller = pane();
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

  /* Only your line carries a marker; the reply that follows is a sibling. */
  it('stays on the exchange being read while the next one peeks in at the bottom', async () => {
    await page.viewport(1400, 900);
    render(<RailPane turns={railTurns(8)} />);
    await frame();
    await scrollPaneTo(0);
    await scrollPaneTo(pane().scrollTop + markers()[3].getBoundingClientRect().top
      - pane().getBoundingClientRect().bottom + 5);

    expect(markers()[3].getBoundingClientRect().top)
      .toBeLessThan(pane().getBoundingClientRect().bottom);
    expect(currentDot()).toBe(2);
  });

  it('keeps the mark on an exchange taller than the pane', async () => {
    await page.viewport(1400, 900);
    render(<RailPane turns={railTurns(8)} paneHeight={220} />);
    await frame();
    await scrollPaneTo(0);
    await scrollPaneTo(pane().scrollTop + markers()[2].getBoundingClientRect().top
      - pane().getBoundingClientRect().top + 100);

    const paneBox = pane().getBoundingClientRect();
    for (const marker of markers()) {
      const top = marker.getBoundingClientRect().top;
      expect(top < paneBox.top || top > paneBox.bottom).toBe(true);
    }
    expect(currentDot()).toBe(2);
  });

  /* Near the end the browser clamps the scroll, so the pressed exchange never reaches the top; the edge has slid to the pane's bottom. (Indices are the code's, from zero.) */
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

  /* Thirteen samples over the last dozen pixels of scroll, where a threshold instead of a slide jumped three exchanges. */
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

  /* `onFocus` moves the roving stop, and a mouse press fires `focus` too. */
  it('gives the tab stop back to the lit dot when the lit dot moves', async () => {
    await page.viewport(1400, 900);
    render(<RailPane turns={railTurns(9, 6)} />);
    await frame();
    await scrollPaneTo(0);
    const stops = () => dots().map((dot) => dot.getAttribute('tabindex'));
    expect(stops()[0]).toBe('0');

    /* Chromium focuses the button it is pressing; `HTMLElement.click()` alone does not. */
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

  /* The case above holds focus in the rail, so the stop is carried by the focus transfer; this is the `setRoved(null)` path. */
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

    const scroller = pane();
    scroller.setAttribute('tabindex', '-1');
    scroller.focus();
    expect(railTrack().contains(document.activeElement)).toBe(false);

    await scrollPaneTo(scroller.scrollHeight);

    expect(currentDot()).toBe(8);
    expect(stops()[8]).toBe('0');
    expect(stops().filter((stop) => stop === '0')).toHaveLength(1);
  });

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

  /* A resize dispatches no `scroll`: the composer shrinking or the window growing closes the distance silently. */
  it('follows again after the pane grows around a parked reader', async () => {
    await page.viewport(1400, 900);
    const { rerender } = render(<RailPane turns={railTurns(8)} paneHeight={400} />);
    await frame();
    const remaining = () => pane().scrollHeight - pane().scrollTop - pane().clientHeight;
    await scrollPaneTo(pane().scrollHeight - pane().clientHeight - 300);
    expect(remaining()).toBeCloseTo(300, 0);

    rerender(<RailPane turns={railTurns(8)} paneHeight={650} />);
    await settle();
    expect(remaining()).toBeLessThanOrEqual(64);

    rerender(<RailPane turns={[...railTurns(8), {
      id: 'late', author: 'agent' as const, text: `Later. ${LINE.repeat(4)}`, atMs: 99_000,
    }]} paneHeight={650} />);
    await settle();

    const scroller = pane();
    expect(scroller.scrollTop).toBe(scroller.scrollHeight - scroller.clientHeight);
  });

  it('bounds the rail by the pane and scrolls the lit dot into its own view', async () => {
    await page.viewport(1400, 900);
    render(<RailPane turns={railTurns(40, 8)} />);
    await frame();
    const scroller = pane();
    const track = railTrack();
    expect(dots()).toHaveLength(40);

    expect(track.getBoundingClientRect().height).toBeLessThanOrEqual(scroller.clientHeight);
    expect(track.scrollHeight).toBeGreaterThan(track.clientHeight + 1);

    await scrollPaneTo(scroller.scrollHeight);
    expect(currentDot()).toBe(39);
    const last = dots()[39].getBoundingClientRect();
    const box = track.getBoundingClientRect();
    expect(last.top).toBeGreaterThanOrEqual(box.top - 1);
    expect(last.bottom).toBeLessThanOrEqual(box.bottom + 1);

    await scrollPaneTo(0);
    expect(currentDot()).toBe(0);
    const first = dots()[0].getBoundingClientRect();
    const back = track.getBoundingClientRect();
    expect(first.top).toBeGreaterThanOrEqual(back.top - 1);
    expect(first.bottom).toBeLessThanOrEqual(back.bottom + 1);

    expect(back.bottom).toBeLessThanOrEqual(scroller.getBoundingClientRect().bottom + 1);
  });

  /* Nothing keyed on the lit exchange re-runs when the drawer shrinks, so this needs its own trigger. */
  it('brings the lit dot back into view when the pane shrinks under it', async () => {
    await page.viewport(1400, 900);
    const { rerender } = render(<RailPane turns={railTurns(40)} paneHeight={400} />);
    await frame();
    await scrollPaneTo(0);
    await scrollPaneTo(pane().scrollTop + markers()[30].getBoundingClientRect().top
      - pane().getBoundingClientRect().top);
    expect(currentDot()).toBe(30);
    expect(dots()[30].getBoundingClientRect().bottom)
      .toBeCloseTo(railTrack().getBoundingClientRect().bottom, 0);

    rerender(<RailPane turns={railTurns(40)} paneHeight={340} />);
    await settle();

    expect(currentDot()).toBe(30);
    const dot = dots()[30].getBoundingClientRect();
    const box = railTrack().getBoundingClientRect();
    expect(dot.top).toBeGreaterThanOrEqual(box.top - 1);
    expect(dot.bottom).toBeLessThanOrEqual(box.bottom + 1);
    expect(box.bottom).toBeLessThanOrEqual(pane().getBoundingClientRect().bottom + 1);
  });

  /* The `ResizeObserver` reports zero-height observations too (a `display: none` ancestor); it heals on the next non-zero frame, so it must be asserted while hidden. */
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

    expect(currentDot()).toBe(0);
    expect(getComputedStyle(railTrack()).blockSize).toBe(bound);

    scroller.style.display = '';
    await settle();

    expect(currentDot()).toBe(0);
    expect(getComputedStyle(railTrack()).blockSize).toBe(bound);
  });

  /* A slide keyed on the remaining scroll alone is already `paneHeight − overflow` of the way down at the top of a transcript that overflows by less than a pane. */
  it('lights the first dot at the top of a transcript that overflows by less than a pane', async () => {
    await page.viewport(1400, 900);
    render(<RailPane turns={railTurns(6, 0)} paneHeight={2000} />);
    await frame();
    /* The transcript's own box, not the pane's `scrollHeight`: a pane taller than its content reports its own height. */
    const content = document.querySelector<HTMLElement>('[data-nc-rail-pane-inner]')!
      .getBoundingClientRect().height;

    document.body.replaceChildren();
    render(<RailPane turns={railTurns(6, 0)} paneHeight={content - 60} />);
    await frame();

    const scroller = pane();
    const overflow = scroller.scrollHeight - scroller.clientHeight;
    expect(overflow).toBeGreaterThan(0);
    expect(overflow).toBeLessThan(scroller.clientHeight);
    expect(dots()).toHaveLength(6);

    expect(scroller.scrollTop).toBe(overflow);
    await scrollPaneTo(0);
    expect(currentDot()).toBe(0);

    dots()[1].click();
    await settle();
    expect(currentDot()).toBe(0);
  });

  /* Nothing re-runs the rail effect for a change of height (its deps are the exchanges), so the observer must be installed before the zero-height guard. */
  it('lights the rail after a pane that mounted with no layout gains some', async () => {
    await page.viewport(1400, 900);
    const { rerender } = render(<RailPane turns={railTurns(8)} paneHeight={0} />);
    await frame();
    expect(pane().clientHeight).toBe(0);
    expect(currentDot()).toBe(-1);

    rerender(<RailPane turns={railTurns(8)} paneHeight={400} />);
    await settle();

    expect(currentDot()).toBe(7);
    expect(Number.parseFloat(getComputedStyle(railTrack()).blockSize)).toBeGreaterThan(0);

    await scrollPaneTo(0);
    expect(currentDot()).toBe(0);
    await scrollPaneTo(pane().scrollTop + markers()[3].getBoundingClientRect().top
      - pane().getBoundingClientRect().top);
    expect(currentDot()).toBe(3);
  });

  /* A roving group whose focused element is not its tab stop leaves the next Tab from a `tabIndex="-1"` element. */
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
    expect(dots().filter((dot) => dot.getAttribute('tabindex') === '0')).toHaveLength(1);
  });

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

  /* The one case that drives the real `<Drawer>`: the seam is rendered, found by `drawerSeamAround`, animates with the card, and leaves with it. */
  it('enters and leaves with the drawer, and takes the dots with it', async () => {
    await page.viewport(1400, 900);
    const host = document.createElement('div');
    host.style.cssText = 'position:relative;block-size:600px;inline-size:900px';
    document.body.append(host);

    function Harness({ open }: { open: boolean }) {
      return (
        <Drawer open={open} title="Ship the rewrite" onClose={() => {}}>
          <ChatThread cards={{}} stalled={false} conversation={railConversation()} turns={railTurns(8)} />
        </Drawer>
      );
    }
    const view = render(<Harness open />, { container: host });
    await frame();

    const seam = host.querySelector<HTMLElement>('[data-nc-drawer-seam]')!;
    const card = host.querySelector<HTMLElement>('[data-nc-drawer]')!;
    expect(seam).not.toBeNull();
    expect(dots()).toHaveLength(8);
    expect(seam.contains(railTrack())).toBe(true);
    expect(card.contains(railTrack())).toBe(false);

    const timing = (element: Element) => {
      const style = getComputedStyle(element);
      return `${style.animationName} ${style.animationDuration} ${style.animationTimingFunction}`;
    };
    expect(timing(seam)).toBe(timing(card));
    const enteringName = getComputedStyle(seam).animationName;
    expect(enteringName).not.toBe('none');

    view.rerender(<Harness open={false} />);
    await frame();
    const leavingCard = host.querySelector<HTMLElement>('[data-nc-drawer]')!;
    const leavingSeam = host.querySelector<HTMLElement>('[data-nc-drawer-seam]')!;
    expect(getComputedStyle(leavingSeam).animationName).not.toBe(enteringName);
    expect(timing(leavingSeam)).toBe(timing(leavingCard));

    /* The exit is one `--motion-medium`; wait it out rather than guessing. */
    const startedAt = performance.now();
    while (host.querySelector('[data-nc-drawer]') !== null
      && performance.now() - startedAt < 2_000) await pause(20);

    expect(host.querySelector('[data-nc-drawer]')).toBeNull();
    expect(host.querySelector('[data-nc-drawer-seam]')).toBeNull();
    expect(dots()).toHaveLength(0);
    host.remove();
  });
});

/* Astryx's `Markdown` blocks take family, size and leading from its own variables; `.reply` overrides the three, and an upstream rename would disconnect it silently. */
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

  /** The block Astryx painted the words into — never `.reply` itself, which carries the plain declarations and would make the case vacuous. */
  /* Two functions rather than one taking a selector: `architecture/no-class-dom-query` requires every runtime query to be a static string. */
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

    const wanted = probe({
      fontFamily: 'var(--font-serif)',
      fontSize: 'var(--text-md)',
      lineHeight: 'var(--leading-loose)',
    });
    expect(painted.fontFamily).toBe(wanted.fontFamily);
    expect(painted.fontSize).toBe(wanted.fontSize);
    expect(painted.lineHeight).toBe(wanted.lineHeight);

    expect(painted.fontFamily).not.toBe(probe({ fontFamily: 'var(--font-sans)' }).fontFamily);
  });

  /* Headings take `--font-family-heading`, which the app-wide bridge maps to the display sans. */
  it('paints the reply’s own headings in the same serif, not the display sans', () => {
    render(<RailPane turns={MARKDOWN_REPLY} />);
    const heading = getComputedStyle(paintedHeading());

    expect(heading.fontFamily).toBe(probe({ fontFamily: 'var(--font-serif)' }).fontFamily);
    expect(heading.fontFamily).not.toBe(probe({ fontFamily: 'var(--font-display)' }).fontFamily);
  });
});

/* A flex line fills and wraps before it shrinks, and `.activityTarget`'s `overflow: hidden` zeroes its minimum size; jsdom computes no layout, so only this tier can count rows. */
describe('the activity line’s row count, as the engine lays it out', () => {
  /** `clip()` cuts at `ACTIVITY_TARGET_MAX`, so 64 characters is the widest noun that can reach this component. */
  const LONG_TARGET = 'cargo clippy --workspace --all-targets --all-features -- -D war…';

  function activity(overrides: Partial<ConversationActivity>): ConversationActivity {
    return {
      id: 'a1', author: 'activity', verb: 'Ran', target: LONG_TARGET, state: 'done',
      durationMs: null, detail: null, tool: null, atMs: 0, ...overrides,
    };
  }

  /** The activity paragraphs, in order; the spans inside carry only hashed module classes, so they are reached positionally. */
  const lines = () => [...document.querySelectorAll<HTMLElement>('p[data-nc-state]')];

  const rowOf = (line: HTMLElement) => line.children[0] as HTMLElement;
  const verbOf = (line: HTMLElement) => rowOf(line).children[0] as HTMLElement;
  const nounOf = (line: HTMLElement) => rowOf(line).children[1] as HTMLElement;

  /** Same row when vertical extents overlap, not when tops are equal: the spans are set in different families and aligned on their baselines. */
  const sameRow = (a: HTMLElement, b: HTMLElement) => {
    const [x, y] = [a.getBoundingClientRect(), b.getBoundingClientRect()];
    return x.top < y.bottom && y.top < x.bottom;
  };

  it('keeps a long done line on one row, ellipsized beside the verb', async () => {
    await page.viewport(1400, 900);
    expect(LONG_TARGET).toHaveLength(64);
    render(<RailPane turns={[
      activity({}),
      { id: 'reply', author: 'agent', text: 'Next check', atMs: 0 },
      activity({ id: 'a2', target: 'ls' }),
    ]} />);
    await frame();

    const [long, short] = lines();
    expect(long.getBoundingClientRect().height)
      .toBe(short.getBoundingClientRect().height);
    expect(sameRow(nounOf(long), verbOf(long))).toBe(true);
    expect(nounOf(long).clientWidth).toBeLessThan(nounOf(long).scrollWidth);
  });

  it('lays a failed line out as exactly two rows, whatever else is on it', async () => {
    await page.viewport(1400, 900);
    render(<RailPane turns={[
      activity({ state: 'failed', durationMs: 8_400, detail: 'error: no test specified' }),
      { id: 'reply', author: 'agent', text: 'Next check', atMs: 0 },
      activity({ id: 'a2', target: 'ls' }),
    ]}
    />);
    await frame();

    const [failed, done] = lines();
    const row = rowOf(failed);
    const items = [...row.children] as HTMLElement[];
    expect(items.map((item) => item.textContent))
      .toEqual(['Ran', LONG_TARGET, 'Failed', '8.4s']);

    for (const item of items.slice(1)) expect(sameRow(item, verbOf(failed))).toBe(true);

    const detail = failed.children[1] as HTMLElement;
    expect(detail.textContent).toBe('error: no test specified');
    expect(detail.getBoundingClientRect().top)
      .toBeGreaterThanOrEqual(row.getBoundingClientRect().bottom);

    const box = failed.getBoundingClientRect();
    expect(box.height).toBeCloseTo(
      row.getBoundingClientRect().height + detail.getBoundingClientRect().height, 1,
    );
    expect(row.getBoundingClientRect().height)
      .toBeCloseTo(done.getBoundingClientRect().height, 1);

    expect(nounOf(failed).clientWidth).toBeLessThan(nounOf(failed).scrollWidth);
  });
});

/** WCAG 2.x relative luminance from what `getComputedStyle` hands back: Chromium serialises these tokens as `oklch(L C H)`, so the conversion to linear sRGB happens here. */
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


describe('Send and Stop circular contours', () => {
  it.each(['Send', 'Stop'] as const)('keeps %s full even inside the concentric composer radius', async (label) => {
    await page.viewport(1180, 800);
    render(label === 'Send' ? <Card /> : <ChatComposer onSend={vi.fn()} onStop={vi.fn()} onNewConversation={vi.fn()} />);
    const button = await page.getByRole('button', { name: label, exact: true }).findElement();
    const box = button.getBoundingClientRect();
    expect(box.width).toBe(box.height);
    expect(getComputedStyle(button).borderRadius).toBe('999px');
  });
});


describe('Mobile conversation contours', () => {
  it('keeps the real chat composer and message radius rounded on the full mobile page', async () => {
    await page.viewport(390, 844);
    render(<Drawer open title="A very long conversation title that must remain within the mobile header"
      mobileBackLabel="Conversations" onClose={vi.fn()} footer={<ChatComposer onSend={vi.fn()} />}>
      <p>Existing conversation content.</p>
    </Drawer>);
    const drawer = document.querySelector<HTMLElement>('[data-nc-drawer]')!;
    await Promise.all(drawer.getAnimations().map((animation) => animation.finished));
    const input = await page.getByRole('textbox', { name: 'Message' }).findElement();
    const composer = input.closest('[data-density]')!.firstElementChild!;
    expect(getComputedStyle(composer).borderRadius).toBe('16px');
    expect(getComputedStyle(composer).backgroundColor).not.toBe(getComputedStyle(drawer).backgroundColor);
    expect(getComputedStyle(drawer).getPropertyValue('--radius-chat').trim()).toBe('16px');
    const heading = await page.getByRole('heading', { name: /^A very long conversation/ }).findElement();
    expect(getComputedStyle(heading).fontSize).toBe('16px');
    expect(heading.getBoundingClientRect().right).toBeLessThanOrEqual(window.innerWidth);
    expect(drawer.getBoundingClientRect().height).toBe(window.innerHeight);
  });
});
