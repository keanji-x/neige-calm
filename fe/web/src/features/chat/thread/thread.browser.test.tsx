/* The composer, the exchange rail and the reply's type, measured against a real rendering engine. */
import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
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

afterEach(() => { cleanup(); document.body.replaceChildren(); });

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
function ImageOnlyCard({ sendWaiting = false, disabled = sendWaiting }: { sendWaiting?: boolean; disabled?: boolean }) {
  return (
    <div style={{ position: 'absolute', insetBlock: 20, insetInlineEnd: 24, inlineSize: 396 }}>
      <div
        style={{
          ['--nc-card-inset' as string]: '8px',
          ['--nc-card-radius' as string]: '16px',
        }}
      >
        <ChatComposer onSend={vi.fn()} allowEmptyText onNewConversation={vi.fn()} disabled={disabled} sendWaiting={sendWaiting} />
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

  it('turns Send into a spinner on the same round chip while a pressed message waits to go', async () => {
    await page.viewport(1400, 900);
    /* The same composer, disabled as it is while the message waits, showing Send. */
    const { unmount } = render(<ImageOnlyCard disabled />);
    const send = document.querySelector<HTMLElement>('button[aria-label="Send"]')!.getBoundingClientRect();
    unmount();
    render(<ImageOnlyCard sendWaiting />);
    const waiting = screen.getByRole('button', { name: 'Sending…' });
    expect(waiting.hasAttribute('disabled')).toBe(true);
    expect(waiting.getAttribute('aria-busy')).toBe('true');
    expect(document.querySelector('button[aria-label="Send"], [data-nc-send-attachment]')).toBeNull();
    const box = waiting.getBoundingClientRect();
    expect([box.width, box.height, box.right, box.bottom]).toEqual([send.width, send.height, send.right, send.bottom]);
    expect(parseFloat(getComputedStyle(waiting).borderTopLeftRadius)).toBeGreaterThanOrEqual(box.height / 2);
    const probe = document.createElement('div');
    probe.style.backgroundColor = 'var(--surface-chip)';
    document.body.append(probe);
    expect(getComputedStyle(waiting).backgroundColor).toBe(getComputedStyle(probe).backgroundColor);
    probe.remove();
  });

  it('shows edit mode as one bar above the field and Replace on Send\'s round chip, at phone width too', async () => {
    await page.viewport(390, 844);
    const { unmount } = render(<ImageOnlyCard disabled />);
    const send = document.querySelector<HTMLElement>('button[aria-label="Send"]')!.getBoundingClientRect();
    unmount();
    render(<div style={{ position: 'absolute', insetBlock: 20, insetInlineEnd: 24, inlineSize: 396 }}>
      <div style={{ ['--nc-card-inset' as string]: '8px', ['--nc-card-radius' as string]: '16px' }}>
        <ChatComposer onSend={vi.fn()} allowEmptyText onNewConversation={vi.fn()} disabled
          editing={{ preview: 'Draft the changelog entry from this screenshot of the diff, and keep it short.', onCancel: vi.fn() }} />
      </div>
    </div>);
    const bar = document.querySelector<HTMLElement>('[data-nc-edit-bar]')!;
    const preview = bar.children[1] as HTMLElement;
    expect(preview.scrollWidth).toBeGreaterThan(preview.clientWidth);
    expect(bar.getBoundingClientRect().height).toBeLessThanOrEqual(28);
    expect(bar.getBoundingClientRect().bottom).toBeLessThanOrEqual(document.querySelector('[contenteditable]')!.getBoundingClientRect().top);
    expect(screen.getByRole('button', { name: 'Cancel edit' }).getBoundingClientRect().right)
      .toBeLessThanOrEqual(composer().getBoundingClientRect().right);
    /* The bar's row sits above the field, so the footer moves down; size and edge are Send's. */
    const replace = screen.getByRole('button', { name: 'Replace message' });
    const box = replace.getBoundingClientRect();
    expect([box.width, box.height, box.right]).toEqual([send.width, send.height, send.right]);
    expect(document.querySelector('[data-nc-send-attachment]')).toBeNull();
    expect(document.documentElement.scrollWidth).toBeLessThanOrEqual(390);
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
    <><button type="button" style={{ position: 'fixed', insetBlockStart: 0, insetInlineEnd: 0 }}>Outside navigation</button><div
      data-nc-rail-host=""
      style={{
        position: 'relative',
        containerType: 'inline-size',
        blockSize: paneHeight + DRAWER_BLOCK_INSETS,
        inlineSize: 900,
        ['--conversation-span' as string]: `${conversationSpan}px`,
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
    </div></>
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

/* Measure the revealed error, not the hidden disclosure or enclosing live region. */
describe('a failed turn in a real engine', () => {
  it('wraps the current failure and replaces it with one completed row', async () => {
    const you: ConversationTurn = { id: 'you-1', author: 'you', text: 'Summarise everything.', atMs: 0 };
    const failed: ConversationTurnOutcome = {
      id: 'outcome-1', author: 'turn', elapsedMs: null, turnId: 'turn-1', status: 'failed',
      message: 'The conversation exceeded the model\'s context window and the request was rejected before any output was produced.',
      text: 'The conversation exceeded the model\'s context window and the request was rejected before any output was produced.',
      code: 'contextWindowExceeded', atMs: 0,
    };
    const completed: ConversationTurnOutcome = {
      id: 'outcome-2', author: 'turn', elapsedMs: null, turnId: 'turn-2', status: 'completed', atMs: 0,
    };
    const { rerender } = render(<RailPane turns={[you, failed]} />);
    await frame();

    const outcomes = document.querySelectorAll<HTMLElement>('[data-nc-turn="outcome"]');
    expect(outcomes).toHaveLength(1);
    const outcome = outcomes[0];
    expect(outcome.dataset['ncTurnOutcome']).toBe('failed');
    expect(screen.getByText('Failed', { exact: true }).checkVisibility()).toBe(true);
    const disclosure = screen.getByRole('button', { name: 'Failed', expanded: false });
    disclosure.focus();
    await userEvent.keyboard('{Enter}');
    expect(disclosure.getAttribute('aria-expanded')).toBe('true');
    const message = outcome.querySelector<HTMLElement>('[data-nc-turn-outcome-message]')!;
    expect(message.textContent).toBe(failed.text);
    expect(message.checkVisibility()).toBe(true);
    const labelBox = disclosure.getBoundingClientRect();
    const messageBox = message.getBoundingClientRect();
    expect(messageBox.top).toBeGreaterThanOrEqual(labelBox.bottom);
    expect(messageBox.width).toBeLessThanOrEqual(outcome.getBoundingClientRect().width + 1);
    expect(messageBox.height).toBeGreaterThan(Number.parseFloat(getComputedStyle(message).lineHeight) * 1.5);
    expect(outcome.querySelector('[data-nc-turn-outcome-hint]')?.textContent)
      .toBe('The conversation no longer fits in the model’s context window.');
    rerender(<RailPane turns={[you, failed, { ...you, id: 'you-2' }, completed]} />);
    expect(document.querySelectorAll('[data-nc-current-meta]')).toHaveLength(1);
    expect(screen.getByText('Completed', { exact: true }).checkVisibility()).toBe(true);
    expect(screen.queryByText(failed.text!)).toBeNull();
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
  fireEvent.wheel(pane(), { deltaY: top - pane().scrollTop });
  pane().scrollTop = top;
  await settle();
  pane().dispatchEvent(new Event('scrollend'));
  await settle();
}

const railPreview = () => document.querySelector<HTMLElement>('[data-nc-rail-preview]');

/** Wait out a real interval inside `act`, so a `setTimeout` that lands in
 *  component state is flushed rather than warned about. */
async function pause(ms: number) {
  await act(async () => { await new Promise((resolve) => { setTimeout(resolve, ms); }); });
}

describe('the exchange rail, as the engine lays it out', () => {
  it('spends nothing on the transcript, and lives in the drawer’s seam', async () => {
    await page.viewport(1400, 900);
    render(<RailPane turns={railTurns(2)} />);
    await frame();
    expect(dots()).toHaveLength(2);
    const bare = replies()[0].getBoundingClientRect().left;
    // clientLeft includes the scroll pane's leading gutter on classic-scrollbar platforms.
    expect(Math.round(bare)).toBe(Math.round(pane().getBoundingClientRect().left + pane().clientLeft + 8));

    cleanup(); document.body.replaceChildren();
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
    expect(pitch).toBe(20);
    const dot = dots()[0].getBoundingClientRect();
    expect(Math.round(dot.height)).toBe(pitch);
    expect(Math.round(dot.width)).toBe(24);
    const centres = dots().map((each) => {
      const box = each.getBoundingClientRect();
      return box.top + box.height / 2;
    });
    for (let index = 1; index < centres.length; index += 1) {
      expect(centres[index] - centres[index - 1]).toBeCloseTo(20, 1);
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
  it('magnifies neighboring ink by proximity without moving rows or changing the aimed preview', async () => {
    await page.viewport(1400, 900);
    render(<RailPane turns={promptTurns(10, index => `Prompt ${index}`)} />);
    await frame();
    await scrollPaneTo(0);
    await userEvent.hover(screen.getByRole('button', { name: 'Outside navigation' }));
    const ink = (index: number) => Number.parseFloat(getComputedStyle(dots()[index], '::before').width);
    const hitbox = (dot: HTMLElement) => {
      const { top, left, width, height } = dot.getBoundingClientRect();
      return { top, left, width, height };
    };
    // Scroll changes the active ink; capture its resting state after the native size transition settles.
    await expect.poll(currentDot).toBe(0);
    const railStyle = getComputedStyle(railTrack());
    const expectedResting = dots().map(dot => Number.parseFloat(railStyle.getPropertyValue(
      dot.getAttribute('aria-current') === 'true' ? '--nc-rail-dot-current' : '--nc-rail-dot',
    )));
    await expect.poll(() => dots().map((_, index) => ink(index))).toEqual(expectedResting);
    const boxes = dots().map(hitbox);
    const resting = dots().map((_, index) => ink(index));
    await userEvent.hover(dots()[4]);
    await pause(150);
    expect(ink(4)).toBeCloseTo(8, 1);
    expect(ink(3)).toBeGreaterThan(resting[3]);
    expect(ink(3)).toBeGreaterThan(ink(2));
    expect(ink(2)).toBeGreaterThan(resting[2]);
    expect(ink(3)).toBeCloseTo(ink(5), 1);
    expect(dots().map(hitbox)).toEqual(boxes);
    await pause(150);
    expect(railPreview()?.querySelector('div')?.textContent).toBe('Prompt 4');
    const popup = railPreview();
    const aboveAtCenter = ink(3);
    const belowAtCenter = ink(5);
    await userEvent.hover(dots()[4], { position: { x: 12, y: 19 } });
    await pause(150);
    expect(ink(3)).toBeLessThan(aboveAtCenter);
    expect(ink(5)).toBeGreaterThan(belowAtCenter);
    expect(railPreview()).toBe(popup);
    expect(railPreview()?.querySelector('div')?.textContent).toBe('Prompt 4');
    expect(dots().map(hitbox)).toEqual(boxes);
    await userEvent.hover(dots()[5]);
    await pause(150);
    expect(ink(5)).toBeCloseTo(8, 1);
    expect(ink(6)).toBeGreaterThan(ink(7));
    expect(railPreview()).toBe(popup);
    expect(railPreview()?.querySelector('div')?.textContent).toBe('Prompt 5');
    expect(dots().map(hitbox)).toEqual(boxes);
    await userEvent.hover(screen.getByRole('button', { name: 'Outside navigation' }));
    await pause(150);
    expect(dots().map((_, index) => ink(index))).toEqual(resting);
    expect(dots().map(hitbox)).toEqual(boxes);
  });

  it('recomputes ink under a stationary pointer after scroll, host resize and item changes', async () => {
    await page.viewport(1400, 900);
    const view = render(<RailPane turns={promptTurns(30)} />);
    await frame();
    await scrollPaneTo(0);
    await userEvent.hover(dots()[4]);
    await pause(150);
    const proximity = (index: number) => Number(dots()[index].style.getPropertyValue('--nc-dot-proximity'));
    expect(proximity(4)).toBeGreaterThan(proximity(5));
    railTrack().scrollTop = 20;
    await settle();
    await pause(150);
    expect(proximity(5)).toBeGreaterThan(proximity(4));
    railTrack().parentElement!.style.blockSize = '360px';
    await settle();
    await pause(150);
    expect(proximity(6)).toBeGreaterThan(proximity(5));
    const before = dots().slice(0, 10).map((_, index) => proximity(index));
    view.rerender(<RailPane turns={promptTurns(31)} />);
    await settle();
    await pause(150);
    expect(dots().slice(0, 10).map((_, index) => proximity(index))).toEqual(before);
    await userEvent.hover(screen.getByRole('button', { name: 'Outside navigation' }));
    await pause(150);
    expect(dots().every(dot => dot.style.getPropertyValue('--nc-dot-proximity') === '')).toBe(true);
  });

  it('resumes pointer previews on another row after click, Escape and keyboard selection inside the rail', async () => {
    await page.viewport(1400, 900);
    render(<RailPane turns={promptTurns(12, index => `Prompt ${index}`)} />);
    await frame();
    await userEvent.hover(screen.getByRole('button', { name: 'Outside navigation' }));
    const title = () => railPreview()?.querySelector('div')?.textContent;
    await userEvent.hover(dots()[2]);
    await expect.poll(title).toBe('Prompt 2');
    await userEvent.click(dots()[2]);
    await expect.poll(railPreview).toBeNull();
    await userEvent.hover(dots()[3]);
    await expect.poll(title).toBe('Prompt 3');
    expect(document.querySelectorAll('[data-nc-rail-preview]')).toHaveLength(1);
    expect(railPreview()!.closest('[popover]')!.getBoundingClientRect().width).toBeLessThanOrEqual(272);
    await userEvent.keyboard('{Escape}');
    await expect.poll(railPreview).toBeNull();
    await userEvent.hover(dots()[4]);
    await expect.poll(title).toBe('Prompt 4');
    act(() => { dots()[5].focus(); });
    await expect.poll(title).toBe('Prompt 5');
    await userEvent.keyboard('{Enter}');
    await expect.poll(railPreview).toBeNull();
    await userEvent.hover(dots()[6]);
    await expect.poll(title).toBe('Prompt 6');
    await userEvent.keyboard('{ArrowDown}');
    await expect.poll(title).toBe('Prompt 6');
    expect(dots().every(dot => dot.getBoundingClientRect().height === 20)).toBe(true);
  });

  it('rearms fast selections and grace returns without adding a new cold delay', async () => {
    await page.viewport(1400, 900);
    vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] });
    try {
      render(<RailPane turns={promptTurns(12, index => `Prompt ${index}`)} />);
      await frame();
      const title = () => railPreview()?.querySelector('div')?.textContent;
      act(() => { dots()[2].focus(); });
      await userEvent.keyboard('{Escape}');
      expect(railPreview()).toBeNull();
      await userEvent.hover(screen.getByRole('button', { name: 'Outside navigation' }));
      await userEvent.hover(dots()[2]);
      act(() => { vi.advanceTimersByTime(179); });
      expect(railPreview()).toBeNull();
      // Already focused: this fast click does not cause another native focus-show.
      await userEvent.click(dots()[2]);
      act(() => { vi.advanceTimersByTime(1000); });
      expect(railPreview()).toBeNull();
      await userEvent.hover(dots()[3]);
      act(() => { vi.advanceTimersByTime(0); });
      expect(title()).toBe('Prompt 3');
      await userEvent.click(dots()[3]);
      act(() => { vi.advanceTimersByTime(1000); });
      expect(railPreview()).toBeNull();
      await userEvent.hover(dots()[4]);
      act(() => { vi.advanceTimersByTime(0); });
      expect(title()).toBe('Prompt 4');
      const popup = railPreview();
      await userEvent.hover(screen.getByRole('button', { name: 'Outside navigation' }));
      act(() => { vi.advanceTimersByTime(119); });
      expect(railPreview()).toBe(popup);
      await userEvent.hover(dots()[4]);
      await userEvent.click(dots()[4]);
      expect(railPreview()).toBeNull();
      await userEvent.hover(dots()[5]);
      act(() => { vi.advanceTimersByTime(0); });
      expect(title()).toBe('Prompt 5');
      await userEvent.hover(screen.getByRole('button', { name: 'Outside navigation' }));
      act(() => { vi.advanceTimersByTime(120); });
      expect(railPreview()).toBeNull();
      await userEvent.hover(dots()[6]);
      act(() => { vi.advanceTimersByTime(179); });
      expect(railPreview()).toBeNull();
      act(() => { vi.advanceTimersByTime(1); });
      expect(title()).toBe('Prompt 6');
    } finally {
      cleanup();
      vi.useRealTimers();
    }
  });

  it('scans stable rows with one narrow continuous preview and a short first-entry delay', async () => {
    await page.viewport(1400, 900);
    // Only the hook's JS delays are controlled: pointer moves, CSS motion and RAF stay real.
    vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] });
    try {
      render(<RailPane turns={promptTurns(8, index => `Prompt ${index}`)} conversationSpan={520} />);
      await frame();
      await userEvent.hover(screen.getByRole('button', { name: 'Outside navigation' }));
      const before = dots().map(dot => dot.getBoundingClientRect().top);
      await userEvent.hover(dots()[2]);
      act(() => { vi.advanceTimersByTime(179); });
      expect(railPreview()).toBeNull();
      act(() => { vi.advanceTimersByTime(1); });
      const preview = railPreview();
      expect(preview).not.toBeNull();
      const popup = preview!.closest('[popover]')!;
      expect(popup.getBoundingClientRect().width).toBeLessThanOrEqual(272);
      expect(getComputedStyle(popup).animationDuration).toBe('0.1s');
      expect(preview!.querySelector('div')?.textContent).toBe('Prompt 2');
      expect(dots()[2].getBoundingClientRect().height).toBe(20);
      await userEvent.hover(dots()[3]);
      act(() => { vi.advanceTimersByTime(0); });
      expect(railPreview()).toBe(preview);
      expect(document.querySelectorAll('[data-nc-rail-preview]')).toHaveLength(1);
      expect(preview!.querySelector('div')?.textContent).toBe('Prompt 3');
      expect(dots().map(dot => dot.getBoundingClientRect().top)).toEqual(before);
      await userEvent.hover(preview!);
      act(() => { vi.advanceTimersByTime(160); });
      expect(railPreview()).toBe(preview);
      await userEvent.hover(screen.getByRole('button', { name: 'Outside navigation' }));
      act(() => { vi.advanceTimersByTime(119); });
      expect(railPreview()).toBe(preview);
      await userEvent.hover(dots()[4]);
      act(() => { vi.advanceTimersByTime(0); });
      expect(railPreview()).toBe(preview);
      expect(preview!.querySelector('div')?.textContent).toBe('Prompt 4');
      await userEvent.hover(screen.getByRole('button', { name: 'Outside navigation' }));
      act(() => { vi.advanceTimersByTime(119); });
      expect(railPreview()).toBe(preview);
      act(() => { vi.advanceTimersByTime(1); });
      expect(railPreview()).toBeNull();
    } finally {
      cleanup();
      vi.useRealTimers();
    }
  });

  it('floats the prompt out only after the pointer has rested', async () => {
    await page.viewport(1400, 900);
    render(<RailPane turns={promptTurns(8, (index) => (index === 6 ? OVERLONG_PROMPT : LONG_PROMPT))} />);
    await frame();
    await scrollPaneTo(0);

    const startedAt = performance.now();
    await userEvent.hover(dots()[3]);
    // Exact entry timing is pinned with native pointer moves and controlled JS timers above.
    while (railPreview() === null && performance.now() - startedAt < 2_000) await pause(20);
    const shownAfter = performance.now() - startedAt;
    const preview = railPreview()!;
    expect(preview).not.toBeNull();
    /* A usability ceiling, not a band around the constant. */
    expect(shownAfter).toBeLessThan(1_200);

    const name = dots()[3].getAttribute('aria-label')!;
    expect(preview.querySelector('div')?.textContent).toBe(LONG_PROMPT);
    expect(preview.querySelector('p')?.textContent).toBe('Short.');
    expect(Number.parseFloat(getComputedStyle(preview.querySelector('div')!).fontSize))
      .toBeGreaterThan(Number.parseFloat(getComputedStyle(preview.querySelector('p')!).fontSize));
    expect(name.length).toBeLessThan(LONG_PROMPT.length);
    expect(name).toContain('…');

    expect(preview.closest('[popover]')?.getAttribute('role')).toBe('group');
    expect(getComputedStyle(preview).pointerEvents).toBe('auto');

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
    await pause(250);
    await userEvent.hover(dots()[6]);
    await pause(600);
    await userEvent.hover(railPreview()!);
    await pause(250);
    expect(railPreview()).not.toBeNull();
    const capped = railPreview()!.querySelector('div')!.textContent;
    expect(OVERLONG_PROMPT.length).toBeGreaterThan(240);
    expect(capped).toHaveLength(240);
    expect(capped.endsWith('…')).toBe(true);
    expect(OVERLONG_PROMPT.startsWith(capped.slice(0, -1))).toBe(true);

    await userEvent.hover(replies()[0]);
    await pause(250);
    expect(railPreview()).toBeNull();
  });

  /* Native anchor positioning keeps end-dot cards in the viewport even in a narrow conversation. */
  it('keeps end-dot previews readable inside the viewport', async () => {
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
    expect(first).not.toBeNull();
    expect(first.top).toBeGreaterThanOrEqual(0);
    expect(first.bottom).toBeLessThanOrEqual(window.innerHeight);

    track.scrollTop = track.scrollHeight;
    await settle();
    await userEvent.hover(screen.getByRole('button', { name: 'Outside navigation' }));
    await pause(150);
    act(() => { dots()[29].focus(); });
    await pause(150);
    const last = railPreview()!.getBoundingClientRect();
    expect(last.top).toBeGreaterThanOrEqual(0);
    expect(last.bottom).toBeLessThanOrEqual(window.innerHeight);

    await userEvent.hover(replies()[0]);
    await pause(250);
  });

  /* Native CSS anchor positioning follows its button as the rail scrolls. */
  it('keeps the preview on its dot when the rail scrolls under it', async () => {
    await page.viewport(1400, 900);
    render(<RailPane turns={promptTurns(30, () => 'Ask about the rewrite')} />);
    await frame();
    await scrollPaneTo(0);
    const track = railTrack();
    expect(track.scrollHeight).toBeGreaterThan(track.clientHeight + 80);

    // Park on the shared card so the real pointer does not select another row during scrolling.
    track.scrollTop = 0;
    await userEvent.hover(dots()[8]);
    await pause(300);
    await userEvent.hover(railPreview()!);
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

  it('shows an ordinal and the reply for an exchange with an empty prompt', async () => {
    await page.viewport(1400, 900);
    render(<RailPane turns={promptTurns(8, index => index === 2 ? '   ' : LONG_PROMPT)} />);
    await frame();
    await userEvent.hover(dots()[2]);
    await pause(600);
    expect(railPreview()?.querySelector('div')?.textContent).toBe('Exchange 3');
    expect(railPreview()?.querySelector('p')).not.toBeNull();
  });

  /* A touchscreen's first pointer event on a control is the press; a laptop with a touchscreen reports `pointer: fine`, so the component's own check is the guard. */
  it('does not float the prompt out for a touch pointer', async () => {
    await page.viewport(1400, 900);
    render(<RailPane turns={promptTurns()} />);
    await frame();
    await userEvent.hover(screen.getByRole('button', { name: 'Outside navigation' }));
    await pause(600);
    expect(railPreview()).toBeNull();

    const enter = (pointerType: string) => {
      dots()[3].dispatchEvent(new PointerEvent('pointerenter', { bubbles: true, pointerType }));
      railTrack().dispatchEvent(new PointerEvent('pointerenter', { pointerType }));
      railTrack().dispatchEvent(new MouseEvent('mouseenter'));
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

  /* Geometry can bring history near the tail without a reader choosing it. */
  it('keeps a parked reader after the pane grows around them', async () => {
    await page.viewport(1400, 900);
    const { rerender } = render(<RailPane turns={railTurns(8)} paneHeight={400} />);
    await frame();
    const remaining = () => pane().scrollHeight - pane().scrollTop - pane().clientHeight;
    await scrollPaneTo(pane().scrollHeight - pane().clientHeight - 300);
    expect(remaining()).toBeCloseTo(300, 0);

    rerender(<RailPane turns={railTurns(8)} paneHeight={650} />);
    await settle();
    expect(remaining()).toBeLessThanOrEqual(64);
    const parked = pane().scrollTop;

    rerender(<RailPane turns={[...railTurns(8), {
      id: 'late', author: 'agent' as const, text: `Later. ${LINE.repeat(4)}`, atMs: 99_000,
    }]} paneHeight={650} />);
    await settle();

    const scroller = pane();
    expect(scroller.scrollTop).toBe(parked);
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

    cleanup(); document.body.replaceChildren();
    /* An overflow smaller than the second marker's distance below the first, so no scroll the pane can make brings it to the edge. */
    render(<RailPane turns={railTurns(6, 0)} paneHeight={content - 30} />);
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
    act(() => { dots()[0].focus(); });
    await userEvent.keyboard('{ArrowDown}');
    await expect.poll(railPreview).not.toBeNull();
    expect(dots()[1].matches(':focus-visible')).toBe(true);
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

  it('dismisses the standard preview with Escape and selection while keeping the drawer open', async () => {
    await page.viewport(1400, 900);
    const onClose = vi.fn();
    render(<div style={{ position: 'relative', blockSize: 600, inlineSize: 900 }}>
      <Drawer open title="Ship the rewrite" onClose={onClose}>
        <ChatThread canContinue={false} cards={{}} stalled={false} conversation={railConversation()} turns={railTurns(8)} />
        <textarea aria-label="Preview test composer" />
      </Drawer>
    </div>);
    await pause(300);
    act(() => { dots()[0].focus(); });
    await frame();
    expect(railPreview()).not.toBeNull();
    await userEvent.keyboard('{Escape}');
    await frame();
    expect(railPreview()).toBeNull();
    expect(onClose).not.toHaveBeenCalled();
    await userEvent.keyboard('{ArrowDown}');
    await frame();
    expect(railPreview()).not.toBeNull();
    await userEvent.keyboard('{Enter}');
    await frame();
    expect(railPreview()).toBeNull();
    expect(onClose).not.toHaveBeenCalled();
    act(() => { screen.getByRole('textbox', { name: 'Preview test composer' }).focus(); });
    await userEvent.hover(dots()[3]);
    await pause(600);
    expect(railPreview()).not.toBeNull();
    await userEvent.keyboard('{Escape}');
    await frame();
    expect(railPreview()).toBeNull();
    expect(onClose).not.toHaveBeenCalled();
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
          <ChatThread canContinue={false} cards={{}} stalled={false} conversation={railConversation()} turns={railTurns(8)} />
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
      const effect = element.getAnimations()[0].effect as KeyframeEffect;
      return { duration: effect.getTiming().duration, easing: effect.getTiming().easing };
    };
    const target = (element: Element) => Number((element.getAnimations()[0].effect as KeyframeEffect).getKeyframes().at(-1)!.opacity);
    expect(timing(seam)).toEqual(timing(card));
    expect(target(seam)).toBe(1);
    await Promise.all(card.getAnimations().map(animation => animation.finished));

    view.rerender(<Harness open={false} />);
    const leavingCard = host.querySelector<HTMLElement>('[data-nc-drawer]')!;
    const leavingSeam = host.querySelector<HTMLElement>('[data-nc-drawer-seam]')!;
    expect(target(leavingSeam)).toBe(0);
    expect(timing(leavingSeam)).toEqual(timing(leavingCard));

    /* The shared physical solver decides settlement; wait for actual disposal. */
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

/* A lone call is Astryx's single-call row, the same row a group draws; jsdom computes no layout, so only this tier can count rows. */
describe('a lone tool call’s row, as the engine lays it out', () => {
  /** `clip()` cuts at `ACTIVITY_TARGET_MAX`, so 64 characters is the widest noun that can reach this component. */
  const LONG_TARGET = 'cargo clippy --workspace --all-targets --all-features -- -D war…';
  const REASON = 'error: expected resolveRoute(origin=A, destination=D) to include hop C but received [A, B, D] (core/route/cache.test.ts:42:17)';

  function activity(overrides: Partial<ConversationActivity>): ConversationActivity {
    return {
      id: 'a1', author: 'activity', verb: 'Ran', target: LONG_TARGET, state: 'done',
      durationMs: null, detail: null, tool: null, atMs: 0, ...overrides,
    };
  }

  /** Each lone call's element: the transcript child that carries the call's target. */
  const runOf = (target: string) => [...document.querySelectorAll<HTMLElement>('[data-nc-thread] > [data-nc-entry]')]
    .find((element) => element.textContent?.includes(target))!;
  /** The painted text, not Astryx's visually hidden status announcement. */
  const painted = (run: HTMLElement, text: string) => [...run.querySelectorAll<HTMLElement>('span')]
    .find((span) => span.textContent === text && span.getBoundingClientRect().width > 1)!;

  /** Same row when vertical extents overlap, not when tops are equal: the spans are set in different families and aligned on their baselines. */
  const sameRow = (a: HTMLElement, b: HTMLElement) => {
    const [x, y] = [a.getBoundingClientRect(), b.getBoundingClientRect()];
    return x.top < y.bottom && y.top < x.bottom;
  };

  it('keeps a long done call on one row, ellipsized beside the verb, with its duration once', async () => {
    await page.viewport(1400, 900);
    expect(LONG_TARGET).toHaveLength(64);
    render(<RailPane turns={[
      activity({ durationMs: 4_300 }),
      { id: 'reply', author: 'agent', text: 'Next check', atMs: 0 },
      activity({ id: 'a2', target: 'ls' }),
    ]} />);
    await frame();

    const [long, short] = [runOf(LONG_TARGET), runOf('ls')];
    expect(long.getBoundingClientRect().height).toBe(short.getBoundingClientRect().height);
    const noun = painted(long, LONG_TARGET);
    expect(sameRow(noun, painted(long, 'Ran'))).toBe(true);
    expect(sameRow(painted(long, '4.3s'), noun)).toBe(true);
    expect(long.textContent?.match(/4\.3s/g)).toHaveLength(1);
    expect(noun.clientWidth).toBeLessThan(noun.scrollWidth);
  });

  it('keeps a failed call on one row until it is opened, then shows the whole reason under it', async () => {
    await page.viewport(1400, 900);
    render(<RailPane turns={[
      activity({ state: 'failed', durationMs: 8_400, detail: REASON }),
      { id: 'reply', author: 'agent', text: 'Next check', atMs: 0 },
      activity({ id: 'a2', target: 'ls' }),
    ]}
    />);
    await frame();

    const failed = runOf(LONG_TARGET);
    const done = runOf('ls');
    expect(failed.getBoundingClientRect().height).toBeCloseTo(done.getBoundingClientRect().height, 1);
    const row = screen.getByRole('button', { name: /Ran/, expanded: false });
    expect(failed.contains(row)).toBe(true);
    expect(sameRow(painted(failed, LONG_TARGET), painted(failed, 'Ran'))).toBe(true);
    expect(screen.queryByText(REASON)).toBeNull();

    await userEvent.click(row);
    expect(row.getAttribute('aria-expanded')).toBe('true');
    const detail = screen.getByText(REASON);
    expect(detail.checkVisibility()).toBe(true);
    expect(detail.getBoundingClientRect().top).toBeGreaterThanOrEqual(row.getBoundingClientRect().bottom);
    expect(pane().scrollWidth).toBeLessThanOrEqual(pane().clientWidth);
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

it('paints persisted mention pills in a narrow transcript', async () => {
  await page.viewport(414, 896);
  const text = 'see @`tag:部署` @`area/reports/Deploy notes.md` '
    + '@`area/reports/Deploy notes.md#b_1a2b`';
  render(<div style={{ width: 320 }}><ChatThread canContinue={false} cards={{}} stalled={false}
    conversation={railConversation()}
    turns={[{ id: 'mention-turn', author: 'you', text, atMs: 1 }]} /></div>);
  await expect.element(page.getByText('#部署', { exact: true })).toBeVisible();
  await expect.element(page.getByText('Deploy notes', { exact: true })).toBeVisible();
  await expect.element(page.getByText('Deploy notes › b_1a2b', { exact: true })).toBeVisible();
  const message = document.querySelector<HTMLElement>('[data-nc-turn="you"]')!;
  expect(message.textContent).not.toContain('area/reports/');
  expect(message.scrollWidth).toBeLessThanOrEqual(message.clientWidth);
  await page.screenshot({ path: '../../../../../test-results/planner-sent-mentions.png' });
  await page.viewport(1280, 720);
});


it('keeps long mention pills inside a narrow transcript', async () => {
  await page.viewport(414, 896);
  const name = 'Very long report name '.repeat(12);
  const tag = '部署'.repeat(32);
  render(<div style={{ width: 320 }}><ChatThread canContinue={false} cards={{}} stalled={false}
    conversation={railConversation()}
    turns={[{ id: 'long-mention', author: 'you',
      text: '@`area/reports/' + name + '.md` @`tag:' + tag + '`', atMs: 1 }]} /></div>);
  await expect.element(page.getByText(name, { exact: true })).toBeVisible();
  const message = document.querySelector<HTMLElement>('[data-nc-turn="you"]')!;
  expect(message.scrollWidth).toBeLessThanOrEqual(message.clientWidth);
  const bounds = message.getBoundingClientRect();
  for (const pill of message.querySelectorAll('[data-nc-sent-mention]')) {
    expect(pill.getBoundingClientRect().right).toBeLessThanOrEqual(bounds.right);
  }
  await page.screenshot({ path: '../../../../../test-results/planner-long-mentions.png' });
  await page.viewport(1280, 720);
});

it('renders three distinct command icons and runs compact by keyboard', async () => {
  const compact = vi.fn();
  const send = vi.fn();
  render(<ChatComposer onSend={send} onCompact={compact} onNewConversation={vi.fn()} onSideConversation={vi.fn()} />);
  const field = page.getByRole('combobox', { name: 'Message' });
  await field.fill('/');
  const options = screen.getAllByRole('option');
  expect(options).toHaveLength(3);
  const glyphs = options.map((option) => option.querySelector('svg')?.innerHTML);
  expect(new Set(glyphs).size).toBe(3);
  expect(options.every((option) => option.querySelector('svg')?.getAttribute('aria-hidden') === 'true')).toBe(true);
  await field.fill('/compact');
  await userEvent.keyboard('{Enter}');
  expect(compact).toHaveBeenCalledOnce();
  expect(send).not.toHaveBeenCalled();
});
