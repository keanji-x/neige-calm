// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import {
  CONVERSATION_GAP_MS,
  type Conversation, type ConversationActivity, type ConversationTurn,
} from '../../../../../core/domain/conversation.ts';
import { ChatComposer, ChatThread, EXCHANGE_RAIL_MIN } from './public.tsx';

afterEach(cleanup);

const NOW = 1_760_000_000_000;

function conversation(overrides: Partial<Conversation> = {}): Conversation {
  return {
    id: 'c1', waveId: 'w1', waveTitle: 'Ship the rewrite', title: null, kind: 'codex',
    state: 'idle', updatedAt: NOW, turns: 0,
    ...overrides,
  };
}

function turn(overrides: Partial<ConversationTurn> = {}): ConversationTurn {
  return { id: 't1', author: 'you', text: 'Do the thing.', atMs: NOW, ...overrides };
}

function activity(overrides: Partial<ConversationActivity> = {}): ConversationActivity {
  return {
    id: 'a1', author: 'activity', verb: 'Ran', target: 'npm test', state: 'done', atMs: NOW,
    ...overrides,
  };
}

const PANE_SCROLL_HEIGHT = 1_000;
const PANE_CLIENT_HEIGHT = 400;

/** A drawer pane that actually holds a scroll offset and reports the moves
 *  made to it, so "where the reader last put it" is a thing this tier can
 *  express. `scrollTo` is the reader's own scroll: it moves the offset and
 *  fires the event the component listens to, which is exactly what a wheel
 *  does and what a `scrollTop` write does in a browser. */
function followPane() {
  const pane = document.createElement('div');
  pane.setAttribute('data-nc-drawer-scroll', '');
  let offset = 0;
  const writes: number[] = [];
  Object.defineProperty(pane, 'scrollHeight', {
    configurable: true, value: PANE_SCROLL_HEIGHT,
  });
  Object.defineProperty(pane, 'clientHeight', {
    configurable: true, value: PANE_CLIENT_HEIGHT,
  });
  Object.defineProperty(pane, 'scrollTop', {
    configurable: true,
    get: () => offset,
    set: (value: number) => { offset = value; writes.push(value); },
  });
  document.body.append(pane);
  return {
    pane,
    writes,
    scrollTo: (value: number) => { offset = value; fireEvent.scroll(pane); },
  };
}

describe('ChatThread', () => {
  it('renders the empty state before anything is said', () => {
    render(<ChatThread conversation={conversation()} turns={[]} />);
    expect(screen.getByText('Nothing said yet.')).toBeTruthy();
  });

  it('shows the live mark in an empty pending conversation', () => {
    render(<ChatThread conversation={conversation()} turns={[]} pending />);
    expect(screen.getByLabelText('Working')).toBeTruthy();
  });

  /* A conversation with no live session reads exactly like an idle one, because
     that is all `null` says: no live session was found. It is not a claim that
     the session exited — a card minted two seconds ago arrives the same way. */
  it('renders a stateless conversation exactly like an idle one', () => {
    const { container: idle } = render(
      <ChatThread conversation={conversation({ state: 'idle' })} turns={[turn()]} />,
    );
    const idleHtml = idle.innerHTML;
    cleanup();
    const { container: stateless } = render(
      <ChatThread conversation={conversation({ state: null })} turns={[turn()]} />,
    );
    expect(stateless.innerHTML).toBe(idleHtml);
    expect(screen.queryByLabelText('Working')).toBeNull();
  });

  it('scrolls only the drawer pane when a new turn arrives', () => {
    const pane = document.createElement('div');
    pane.setAttribute('data-nc-drawer-scroll', '');
    Object.defineProperty(pane, 'scrollHeight', { configurable: true, value: 800 });
    const setPaneScroll = vi.fn();
    Object.defineProperty(pane, 'scrollTop', {
      configurable: true,
      get: () => 0,
      set: setPaneScroll,
    });
    const outer = document.createElement('div');
    const setOuterScroll = vi.fn();
    Object.defineProperty(outer, 'scrollTop', {
      configurable: true,
      get: () => 0,
      set: setOuterScroll,
    });
    document.body.append(outer);
    outer.append(pane);
    render(
      <ChatThread conversation={conversation()} turns={[turn()]} />,
      { container: pane },
    );
    expect(setPaneScroll).toHaveBeenCalledWith(800);
    expect(setOuterScroll).not.toHaveBeenCalled();
    outer.remove();
  });

  /*
   * ── Following the newest turn is for a reader who is already there ────────
   *
   * The write used to be unconditional on every change of `turns.length`, and a
   * live turn appends an activity line per action — dozens over a four-minute
   * turn. So anyone who had scrolled back was returned to the bottom within one
   * poll, which is not a nuisance but the removal of the only reason to scroll
   * back at all.
   *
   * The pane here is stateful rather than a spy, because the rule is a fact
   * about *where the reader last put the pane*: the component learns that from
   * the pane's own scroll events, so a mock that cannot be scrolled cannot
   * distinguish the two behaviours.
   */
  it('does not follow a new turn when the reader has scrolled away', () => {
    const { pane, writes, scrollTo } = followPane();
    const { rerender } = render(
      <ChatThread conversation={conversation()} turns={exchangeTurns(3)} />,
      { container: pane },
    );
    /* It opens at the newest turn: that much is unchanged. */
    expect(writes).toEqual([PANE_SCROLL_HEIGHT]);

    scrollTo(100);
    rerender(<ChatThread conversation={conversation()} turns={exchangeTurns(4)} />);

    expect(writes).toEqual([PANE_SCROLL_HEIGHT]);
    expect(pane.scrollTop).toBe(100);
    pane.remove();
  });

  /* And the other half, or the rule would be "never follow": a reader sitting
     at the end still rides the transcript down as it grows. */
  it('follows a new turn for a reader still at the end', () => {
    const { pane, writes, scrollTo } = followPane();
    const { rerender } = render(
      <ChatThread conversation={conversation()} turns={exchangeTurns(3)} />,
      { container: pane },
    );
    scrollTo(PANE_SCROLL_HEIGHT - PANE_CLIENT_HEIGHT);
    rerender(<ChatThread conversation={conversation()} turns={exchangeTurns(4)} />);

    expect(writes).toEqual([PANE_SCROLL_HEIGHT, PANE_SCROLL_HEIGHT]);
    pane.remove();
  });

  /*
   * ── *Load earlier* is a longer transcript, not a newer turn ───────────────
   *
   * Prepending history grows `turns.length` while the newest turn stays exactly
   * what it was, and the write used to fire on the count alone. The reader who
   * presses *Load earlier* is by definition looking for something that just
   * arrived at the *top*, and this is the one case where being at the bottom is
   * not evidence of anything: a transcript that fits in its pane is zero
   * distance from the bottom whatever the reader does, so the flag is
   * unconditionally true and every such reader was thrown to the end of a
   * history they had just asked to see.
   *
   * The reader here is at the bottom on purpose, so this cannot pass by the
   * flag being false for some other reason.
   */
  it('does not follow when older turns are loaded in front', () => {
    const { pane, writes, scrollTo } = followPane();
    const { rerender } = render(
      <ChatThread conversation={conversation()} turns={exchangeTurns(3)} />,
      { container: pane },
    );
    scrollTo(PANE_SCROLL_HEIGHT - PANE_CLIENT_HEIGHT);
    expect(writes).toEqual([PANE_SCROLL_HEIGHT]);

    const earlier = exchangeTurns(2).map((entry) => ({ ...entry, id: `old-${entry.id}` }));
    rerender(
      <ChatThread conversation={conversation()} turns={[...earlier, ...exchangeTurns(3)]} />,
    );

    expect(writes).toEqual([PANE_SCROLL_HEIGHT]);
    /* And the door is not simply nailed shut: the very next real turn still
       takes a reader who is at the end down with it. */
    rerender(
      <ChatThread
        conversation={conversation()}
        turns={[...earlier, ...exchangeTurns(3), turn({ id: 'late', author: 'agent' })]}
      />,
    );
    expect(writes).toEqual([PANE_SCROLL_HEIGHT, PANE_SCROLL_HEIGHT]);
    pane.remove();
  });

  /*
   * ── The count is not the arrival; the last turn's id is ───────────────────
   *
   * `buildTranscript` collapses a trailing `Thought` into the reply that
   * answers it, so `[reasoning, reasoning]` and `[reasoning, reasoning, agent]`
   * both arrive here one entry long with different last ids — the domain's own
   * test spells that out (`core/domain/conversation.test.ts`), and
   * `mergeTranscript` does the same to an optimistic echo. Keyed on the count
   * alone this effect did not run at all for that arrival, so the commonest
   * shape of "the agent answered while its last row was a finished thought"
   * left a reader parked at the bottom looking at the thought.
   */
  it('follows a turn that replaced the last one without changing the count', () => {
    const { pane, writes, scrollTo } = followPane();
    const thinking = [
      turn({ id: 'q1' }),
      turn({ id: 'thought', author: 'agent', text: 'Thought' }),
    ];
    const { rerender } = render(
      <ChatThread conversation={conversation()} turns={thinking} />,
      { container: pane },
    );
    scrollTo(PANE_SCROLL_HEIGHT - PANE_CLIENT_HEIGHT);
    expect(writes).toEqual([PANE_SCROLL_HEIGHT]);

    rerender(
      <ChatThread
        conversation={conversation()}
        turns={[turn({ id: 'q1' }), turn({ id: 'answer', author: 'agent', text: 'hi' })]}
      />,
    );

    expect(writes).toEqual([PANE_SCROLL_HEIGHT, PANE_SCROLL_HEIGHT]);
    pane.remove();
  });

  it('keeps each turn verbatim and marks who wrote it', () => {
    const { container } = render(
      <ChatThread
        conversation={conversation()}
        turns={[turn(), turn({ id: 't2', author: 'agent', text: 'test' })]}
      />,
    );
    const turns = [...container.querySelectorAll('[data-nc-turn]')];
    expect(turns.map((element) => element.getAttribute('data-nc-turn'))).toEqual(['you', 'agent']);
    expect(turns.map((element) => element.textContent)).toEqual(['Do the thing.', 'test']);
  });

  /*
   * The transcript carries no per-turn label and no per-turn timestamp. In a
   * strict alternation those are two lines of chrome per turn restating what
   * the alternation already says; who spoke is carried by register instead.
   * This asserts the *absence*, because a label is the kind of thing that gets
   * added back by reflex.
   */
  it('prints no author label and no time on an unbroken conversation', () => {
    const { container } = render(
      <ChatThread
        conversation={conversation()}
        turns={[
          turn(),
          turn({ id: 't2', author: 'agent', text: 'test', atMs: NOW + 1_000 }),
          turn({ id: 't3', text: 'And this.', atMs: NOW + 2_000 }),
        ]}
      />,
    );
    const text = container.textContent ?? '';
    expect(text).toBe('Do the thing.testAnd this.');
  });

  // A time is a seam, printed where the conversation stopped and started again.
  it('stamps a time where the conversation restarts after a gap', () => {
    const { container } = render(
      <ChatThread
        conversation={conversation()}
        turns={[
          turn(),
          turn({ id: 't2', author: 'agent', text: 'test', atMs: NOW + 1_000 }),
          turn({ id: 't3', text: 'Back.', atMs: NOW + CONVERSATION_GAP_MS + 1_000 }),
        ]}
      />,
    );
    expect(container.textContent).toMatch(/\d{1,2}:\d{2}/);
  });

  it('shows the live mark once while a reply is pending', () => {
    const turns = [turn()];
    const { rerender } = render(<ChatThread conversation={conversation()} turns={turns} pending />);
    expect(screen.getAllByLabelText('Working').length).toBe(1);

    rerender(<ChatThread conversation={conversation()} turns={turns} />);
    expect(screen.queryByLabelText('Working')).toBeNull();
  });

  /*
   * ── The reply is markdown; what you typed is not ──────────────────────────
   *
   * Both halves are asserted because both are decisions, and the second one is
   * the one that gets undone by reflex ("why is only one side rendered?").
   *
   * The reply case asserts *elements*, not text: before this, the same string
   * produced one paragraph with `##` and `-` still in it, and every
   * text-content assertion in this file passed on that. Only the element names
   * separate "rendered as markdown" from "printed the source".
   */
  it('renders the reply as markdown — headings, lists and fenced code', () => {
    const { container } = render(
      <ChatThread
        conversation={conversation()}
        turns={[turn({
          id: 't2',
          author: 'agent',
          text: '## Findings\n\n- first\n- second\n\n```js\nconst a = 1;\n```\n',
        })]}
      />,
    );
    const reply = container.querySelector('[data-nc-turn="agent"]')!;
    /* `##` is one level below `#`, and `#` starts at `h3` — see the case below. */
    expect(reply.querySelector('h4')?.textContent).toBe('Findings');
    expect([...reply.querySelectorAll('li')].map((item) => item.textContent)).toEqual(['first', 'second']);
    expect(reply.querySelector('pre, code')).toBeTruthy();
    /* The source characters are gone, not merely re-styled. */
    expect(reply.textContent).not.toContain('##');
    expect(reply.textContent).not.toContain('```');
  });

  /*
   * `headingLevelStart={3}`: the page owns `<h1>` and its sections own `<h2>`,
   * so a reply's own `#` may not mint either. Asserted separately from the
   * rendering case above because it is a different claim — that markdown is
   * rendered *at a level*, not merely rendered — and a change to the prop
   * leaves that case green.
   */
  it('starts the reply’s headings below the page’s own', () => {
    const { container } = render(
      <ChatThread
        conversation={conversation()}
        turns={[turn({ id: 't2', author: 'agent', text: '# Top' })]}
      />,
    );
    const reply = container.querySelector('[data-nc-turn="agent"]')!;
    expect(reply.querySelector('h1, h2')).toBeNull();
    expect(reply.querySelector('h3')?.textContent).toBe('Top');
  });

  it('leaves what you typed as literal text, markdown or not', () => {
    const { container } = render(
      <ChatThread
        conversation={conversation()}
        turns={[turn({ text: '# not a heading *not* emphasis' })]}
      />,
    );
    const said = container.querySelector('[data-nc-turn="you"]')!;
    expect(said.textContent).toBe('# not a heading *not* emphasis');
    expect(said.querySelector('h1, h2, h3, em, strong')).toBeNull();
  });

  it('states failure in text and exposes activity state through the shared attribute', () => {
    const { container } = render(
      <ChatThread conversation={conversation()} turns={[activity({ state: 'failed' })]} />,
    );
    expect(screen.getByText('Failed')).toBeTruthy();
    expect(container.querySelector('[data-nc-state="failed"]')).toBeTruthy();
    expect(container.querySelector('[data-nc-activity]')).toBeNull();
  });

  it('shows exactly one live mark after a completed activity while live', () => {
    render(<ChatThread conversation={conversation()} turns={[activity()]} pending />);
    expect(screen.getAllByLabelText('Working')).toHaveLength(1);
  });

  it('shows exactly one live mark on a trailing agent turn while live', () => {
    render(
      <ChatThread
        conversation={conversation()}
        turns={[turn({ author: 'agent', text: 'Still working.' })]}
        pending
      />,
    );
    expect(screen.getAllByLabelText('Working')).toHaveLength(1);
  });

  it('shows exactly one live mark on a running activity while live', () => {
    render(
      <ChatThread
        conversation={conversation()}
        turns={[activity({ state: 'running', verb: 'Running' })]}
        pending
      />,
    );
    expect(screen.getAllByLabelText('Working')).toHaveLength(1);
  });

  it('shows no live mark when the conversation is not live', () => {
    render(<ChatThread conversation={conversation()} turns={[activity({ state: 'running' })]} />);
    expect(screen.queryByLabelText('Working')).toBeNull();
  });
});

/*
 * ── The rail of dots ──────────────────────────────────────────────────────
 *
 * What this tier can say: how many dots there are, that they are named, and
 * **which element a press scrolls**. That last one is the whole reason the rail
 * is testable at all here — the failure it guards against (`scrollIntoView`,
 * which walks every ancestor scrollport and pans the page) is visible in jsdom
 * as a write landing on the wrong object, and that is exactly what these mocks
 * read.
 *
 * It can also say **where an exchange starts**, which is a fact about the
 * transcript rather than about the layout — see the consecutive-`you` test
 * below, which is the only input that tells the domain's rule apart from the
 * one a second implementation would reach for.
 *
 * What it cannot say: which dot is lit. That is decided from painted boxes
 * against a real scrollport, and jsdom has neither. The component knows it — it
 * stops at a pane reporting zero height — so here the mark only ever moves on a
 * press.
 */

/** `count` exchanges: your line, then a reply, `count` times over. */
function exchangeTurns(count: number): ConversationTurn[] {
  return Array.from({ length: count }).flatMap((_unused, index) => [
    turn({ id: `you-${index}`, author: 'you', text: `Ask ${index}`, atMs: NOW + index * 2_000 }),
    turn({
      id: `agent-${index}`, author: 'agent', text: `Answer ${index}`, atMs: NOW + index * 2_000 + 1,
    }),
  ]);
}

/**
 * The drawer, as the component finds it — **three boxes now, and the third one
 * is why the rail renders at all.**
 *
 * The pane is `[data-nc-drawer-scroll]`, inside another scrollable box; both
 * report every `scrollTop` write, and the outer one is the ancestor a
 * `scrollIntoView` would have moved. That pair is unchanged.
 *
 * What is added is the card and the **seam**. The rail is no longer a child of
 * the transcript: it is portalled into the strip of page beside the drawer's
 * card (`ui/drawer`, `.seam`), which the component locates with
 * `drawerSeamAround` — `closest('[data-nc-drawer]')`, then that card's parent's
 * `[data-nc-drawer-seam]`. So a fixture that offers a pane and no card has no
 * seam, and a transcript with no seam has no rail *by design*: that is the same
 * answer a transcript rendered outside a drawer gets in the app.
 *
 * jsdom computes no layout, so none of the seam's geometry is reachable here —
 * this box exists to satisfy the *lookup*, and every claim about where the rail
 * lands is in `thread.browser.test.tsx` against a real engine. What this tier
 * still binds is everything the rail does that is not geometry: the naming, the
 * roving stop, the press, and the no-op on a missing marker.
 */
function drawerPane() {
  const outer = document.createElement('div');
  const setOuterScroll = vi.fn();
  Object.defineProperty(outer, 'scrollTop', {
    configurable: true, get: () => 0, set: setOuterScroll,
  });
  const card = document.createElement('div');
  card.setAttribute('data-nc-drawer', '');
  const seam = document.createElement('div');
  seam.setAttribute('data-nc-drawer-seam', '');
  const pane = document.createElement('div');
  pane.setAttribute('data-nc-drawer-scroll', '');
  Object.defineProperty(pane, 'scrollHeight', { configurable: true, value: 800 });
  const setPaneScroll = vi.fn();
  Object.defineProperty(pane, 'scrollTop', {
    configurable: true, get: () => 0, set: setPaneScroll,
  });
  document.body.append(outer);
  outer.append(card, seam);
  card.append(pane);
  return { outer, pane, seam, setOuterScroll, setPaneScroll };
}

function railDots(): HTMLElement[] {
  const rail = screen.queryByRole('group', { name: 'Jump to an exchange' });
  return rail === null ? [] : [...rail.querySelectorAll('button')];
}

/** Give an element a painted box jsdom would otherwise report as all zeroes. */
function boxAt(element: Element, top: number): void {
  element.getBoundingClientRect = () => ({ top, bottom: top + 40, left: 0, right: 300,
    width: 300, height: 40, x: 0, y: top, toJSON: () => ({}) });
}

describe('ChatThread’s exchange rail', () => {
  /*
   * Under the threshold the rail is chrome: four exchanges are about one pane,
   * and the reader who wants the second one can see where it is.
   *
   * **Rendered inside a drawer, which this case now has to be to mean
   * anything.** The rail needs a seam, so a fixture with no drawer produces no
   * rail whatever the count is — and this assertion would then pass on a
   * component with the threshold deleted. The one-above case below is what
   * proves the fixture *can* produce a rail; the two are a pair and neither
   * binds alone.
   */
  it('does not render at all below the threshold', () => {
    const { outer, pane } = drawerPane();
    const { container } = render(
      <ChatThread conversation={conversation()} turns={exchangeTurns(EXCHANGE_RAIL_MIN - 1)} />,
      { container: pane },
    );
    expect(screen.queryByRole('group', { name: 'Jump to an exchange' })).toBeNull();
    /* And nothing else went missing with it: the transcript is all there. */
    expect(container.querySelectorAll('[data-nc-turn]')).toHaveLength(
      (EXCHANGE_RAIL_MIN - 1) * 2,
    );
    outer.remove();
  });

  /*
   * The other half of the pair above, and the case that keeps the whole rail
   * section honest: at exactly the threshold, in the same fixture, the rail is
   * there. Without this the "does not render" case is satisfied by a rail that
   * never renders at all.
   */
  it('renders at exactly the threshold, in the drawer’s seam', () => {
    const { outer, pane, seam } = drawerPane();
    render(
      <ChatThread conversation={conversation()} turns={exchangeTurns(EXCHANGE_RAIL_MIN)} />,
      { container: pane },
    );
    expect(railDots()).toHaveLength(EXCHANGE_RAIL_MIN);
    /* And it is in the seam, not in the transcript — the portal, asserted by
       containment rather than by geometry, which is this tier's whole reach. */
    expect(seam.contains(railDots()[0])).toBe(true);
    expect(pane.querySelector('[data-nc-rail-track]')).toBeNull();
    outer.remove();
  });

  /* No drawer, no seam, no rail — and the transcript is untouched. A transcript
     rendered in place is a transcript without a jump list, which is the honest
     answer: the rail's whole geometry is the drawer's seam. */
  it('renders no rail outside a drawer, and the transcript regardless', () => {
    const { container } = render(
      <ChatThread conversation={conversation()} turns={exchangeTurns(EXCHANGE_RAIL_MIN + 3)} />,
    );
    expect(screen.queryByRole('group', { name: 'Jump to an exchange' })).toBeNull();
    expect(container.querySelectorAll('[data-nc-exchange]')).toHaveLength(EXCHANGE_RAIL_MIN + 3);
  });

  /*
   * One dot per exchange, counted against the *markers the layout groups by*
   * rather than against the number this test asked for. A rail that invented
   * its own idea of where an exchange starts would pass a count against a
   * literal and fail this.
   */
  it('renders exactly one dot per exchange from the threshold up', () => {
    const { outer, pane } = drawerPane();
    const { container } = render(
      <ChatThread conversation={conversation()} turns={exchangeTurns(EXCHANGE_RAIL_MIN + 3)} />,
      { container: pane },
    );
    const markers = container.querySelectorAll('[data-nc-exchange]');
    expect(markers).toHaveLength(EXCHANGE_RAIL_MIN + 3);
    expect(railDots()).toHaveLength(markers.length);
    outer.remove();
  });

  /*
   * ── Where an exchange starts is the domain's answer, not a second one ─────
   *
   * `opensExchange` is "authored by you, **and the turn before it was not**",
   * and the second half is the whole of what a reimplementation drops. Every
   * other fixture in both tiers is a strict you/agent alternation, on which the
   * correct rule and `author === 'you'` are indistinguishable — measured: with
   * both production call sites replaced by `turn.author === 'you'`, the entire
   * web-dom tier stayed green.
   *
   * Two of your turns in a row is not a contrived input. The router appends an
   * optimistic echo of what you just typed while the previous one is still the
   * tail, and `buildTranscript` drops a trailing `Thought`, so two `you` rows
   * end up adjacent either way. The correct answer is one exchange, opened by
   * the first of them; the reimplementation's answer is two, with a
   * `data-nc-exchange` on a row the stylesheet gives no `.exchange` to.
   */
  it('opens one exchange, not two, when you speak twice in a row', () => {
    const turns = exchangeTurns(EXCHANGE_RAIL_MIN);
    turns.splice(1, 0, turn({
      id: 'you-0b', author: 'you', text: 'And also this.', atMs: NOW + 500,
    }));
    const { outer, pane } = drawerPane();
    const { container } = render(
      <ChatThread conversation={conversation()} turns={turns} />, { container: pane },
    );

    /* The marker is on the first of the pair and nowhere else — so the dots and
       the `.exchange` grouping are still the same one element. */
    expect([...container.querySelectorAll('[data-nc-exchange]')]
      .map((marker) => marker.getAttribute('data-nc-exchange')))
      .toEqual(Array.from({ length: EXCHANGE_RAIL_MIN }, (_unused, i) => `you-${i}`));
    expect(railDots()).toHaveLength(EXCHANGE_RAIL_MIN);
    /* And the second line is still in the transcript — this is a claim about
       segmentation, not about dropping a turn. */
    expect(container.querySelectorAll('[data-nc-turn]'))
      .toHaveLength(EXCHANGE_RAIL_MIN * 2 + 1);
    outer.remove();
  });

  /*
   * The prompt is in the name; it is never painted *in the column*, because the
   * 10px the rail costs is a gutter, not a label.
   *
   * The ordinal is in the name too, and it is not decoration: a session with an
   * agent is mostly turns that say "Continue", and five buttons named
   * "Jump to Continue" are five buttons a screen reader cannot tell apart.
   *
   * **And `title` is gone**, asserted rather than merely deleted. It used to
   * carry the prompt on hover, and the floating preview now does — two hovers
   * on one control, on two delays neither of which can wait for the other, is a
   * flicker rather than two aids. The preview needs a rendering engine and
   * lives in the browser tier; what this tier can still say is that the UA
   * tooltip is not also there. */
  it('names each dot with its ordinal and its prompt, and paints no text', () => {
    const { outer, pane } = drawerPane();
    render(
      <ChatThread conversation={conversation()} turns={exchangeTurns(EXCHANGE_RAIL_MIN)} />,
      { container: pane },
    );
    const dots = railDots();
    expect(dots.map((dot) => dot.getAttribute('aria-label'))).toEqual(
      Array.from({ length: EXCHANGE_RAIL_MIN },
        (_unused, index) => `Jump to exchange ${index + 1}: Ask ${index}`),
    );
    expect(dots.some((dot) => dot.hasAttribute('title'))).toBe(false);
    expect(dots.every((dot) => (dot.textContent ?? '') === '')).toBe(true);
    outer.remove();
  });

  /* Same prompt, different button: the names still differ. */
  it('tells identically worded prompts apart', () => {
    const turns = exchangeTurns(EXCHANGE_RAIL_MIN).map((entry) =>
      entry.author === 'you' ? { ...entry, text: 'Continue' } : entry);
    const { outer, pane } = drawerPane();
    render(<ChatThread conversation={conversation()} turns={turns} />, { container: pane });
    const names = railDots().map((dot) => dot.getAttribute('aria-label'));
    expect(new Set(names).size).toBe(EXCHANGE_RAIL_MIN);
    outer.remove();
  });

  /*
   * ── One tab stop, and the arrows inside it ────────────────────────────────
   *
   * Thirty exchanges once meant thirty tab stops between the drawer's edge and
   * the composer. Exactly one dot is in the tab ring at a time now, and it is
   * the one the reader is in.
   */
  it('holds one tab stop and moves it with the arrows', async () => {
    const { outer, pane } = drawerPane();
    render(
      <ChatThread conversation={conversation()} turns={exchangeTurns(EXCHANGE_RAIL_MIN)} />,
      { container: pane },
    );
    const stops = () => railDots().map((dot) => dot.getAttribute('tabindex'));
    expect(stops()).toEqual(['0', '-1', '-1', '-1', '-1']);

    railDots()[0].focus();
    await userEvent.keyboard('{ArrowDown}{ArrowDown}');
    expect(document.activeElement).toBe(railDots()[2]);
    expect(stops()).toEqual(['-1', '-1', '0', '-1', '-1']);

    await userEvent.keyboard('{End}');
    expect(document.activeElement).toBe(railDots()[EXCHANGE_RAIL_MIN - 1]);
    /* And the ends hold rather than wrap: Down at the last dot stays there. */
    await userEvent.keyboard('{ArrowDown}');
    expect(document.activeElement).toBe(railDots()[EXCHANGE_RAIL_MIN - 1]);

    await userEvent.keyboard('{Home}');
    expect(document.activeElement).toBe(railDots()[0]);
    outer.remove();
  });

  /*
   * ── The assertion this component exists to keep honest ───────────────────
   *
   * A press must move **the drawer's own pane** and nothing else. Both the
   * failure modes are visible here: `scrollIntoView` writes no `scrollTop` at
   * all, and any implementation that walks to the wrong scrollport writes to
   * `outer`. The value is checked too, so "wrote something to the right box"
   * is not enough — the marker's top must land on the pane's top.
   */
  it('scrolls the drawer pane to the pressed exchange, and nothing above it', async () => {
    const { outer, pane, setOuterScroll, setPaneScroll } = drawerPane();
    render(
      <ChatThread conversation={conversation()} turns={exchangeTurns(EXCHANGE_RAIL_MIN)} />,
      { container: pane },
    );
    /* The follow-the-newest-turn effect has already written once; the press is
       what this test is about. */
    setPaneScroll.mockClear();
    setOuterScroll.mockClear();

    const markers = [...pane.querySelectorAll('[data-nc-exchange]')];
    boxAt(pane, 100);
    boxAt(markers[2], 340);

    await userEvent.click(railDots()[2]);

    expect(setPaneScroll).toHaveBeenCalledWith(240);
    expect(setOuterScroll).not.toHaveBeenCalled();
    outer.remove();
  });

  /* Pressed dots say so, for the reader who cannot see 6px versus 8px. */
  it('marks the pressed dot as the current one', async () => {
    const { outer, pane } = drawerPane();
    render(
      <ChatThread conversation={conversation()} turns={exchangeTurns(EXCHANGE_RAIL_MIN)} />,
      { container: pane },
    );
    await userEvent.click(railDots()[3]);
    expect(railDots().map((dot) => dot.getAttribute('aria-current')))
      .toEqual([null, null, null, 'true', null]);
    outer.remove();
  });

  /*
   * No marker, no scroll, **and no mark** — the same contract the follow effect
   * keeps. The rail is a faster way through something scrolling already
   * reaches, so a target that is not there has nothing to report and nothing to
   * fall back to.
   *
   * The mark is asserted here and not only the scroll, because the handler used
   * to set it first and look for the marker afterwards: the pane stayed put and
   * the dot lit anyway, which is the one thing a silent no-op must not do —
   * claim, in the one channel a screen reader reads, that the reader was taken
   * somewhere they were not.
   */
  it('does nothing at all when the marker is gone', async () => {
    const { outer, pane, setOuterScroll, setPaneScroll } = drawerPane();
    render(
      <ChatThread conversation={conversation()} turns={exchangeTurns(EXCHANGE_RAIL_MIN)} />,
      { container: pane },
    );
    setPaneScroll.mockClear();
    setOuterScroll.mockClear();
    for (const marker of pane.querySelectorAll('[data-nc-exchange]')) {
      marker.removeAttribute('data-nc-exchange');
    }

    await userEvent.click(railDots()[1]);

    expect(setPaneScroll).not.toHaveBeenCalled();
    expect(setOuterScroll).not.toHaveBeenCalled();
    expect(railDots().some((dot) => dot.getAttribute('aria-current') !== null)).toBe(false);
    outer.remove();
  });

  /*
   * ── How long a pointer must rest before the prompt floats out ─────────────
   *
   * The tier that computes no layout owning the *duration* looks backwards and
   * is exactly right: whether the panel is in the document is not a layout
   * fact. Where it lands is, and that half stays in `thread.browser.test.tsx`.
   * The placement effect runs here too and reads all-zero boxes, which costs
   * nothing — it writes `inset-block-start: 0px`, and nothing in this file
   * reads it.
   *
   * **It is here because the browser tier measured the number against a wall
   * clock, and a shared runner does not hold one still.** That case polled the
   * wait and asserted the elapsed time into `(380, 650)` — a band the shipped
   * 450 sits in the middle of, with 200ms of room for the driver's hover
   * round-trip, the poll's own 20ms of resolution and the runner's scheduling.
   * Run 33380223777 spent 671.6ms of it, 221.6ms of overhead on a 450ms delay,
   * under a mutation of the app's providers that has nothing to do with the
   * rail. A wider band does not repair that, it disarms the band: the floor
   * was the half that rejected 300 and the ceiling the half that rejected 700,
   * and the overhead only ever runs one way — the same 221.6ms that pushed 450
   * past the ceiling lifts a 300ms delay to 520ms, past the floor. Load takes
   * both halves at once, and moving the ceiling gives up the other one.
   *
   * **449 and 450 are written out rather than imported from `public.tsx`.**
   * Importing `RAIL_PREVIEW_DELAY_MS` would make this green for every value of
   * it, which is the exact hole the wall-clock band was widened into. Measured
   * against these literals: at `RAIL_PREVIEW_DELAY_MS = 300` the panel is
   * already up at the 449 step, and at 700 it is still absent one millisecond
   * past 450.
   */
  it('holds the prompt back for the whole delay, then floats it out', () => {
    vi.useFakeTimers();
    try {
      const { outer, pane } = drawerPane();
      render(
        <ChatThread conversation={conversation()} turns={exchangeTurns(EXCHANGE_RAIL_MIN)} />,
        { container: pane },
      );
      const preview = () => document.querySelector('[data-nc-rail-preview]');
      fireEvent.pointerEnter(railDots()[2], { pointerType: 'mouse' });
      expect(preview()).toBeNull();

      act(() => { vi.advanceTimersByTime(449); });
      expect(preview()).toBeNull();
      act(() => { vi.advanceTimersByTime(1); });
      /* The prompt itself, so a panel that mounted empty is not a pass. */
      expect(preview()?.textContent).toBe('Ask 2');
      outer.remove();
    } finally {
      vi.useRealTimers();
    }
  });
});

function messageField(): HTMLElement {
  return screen.getByLabelText('Message');
}

function fieldText(field: HTMLElement): string {
  return field instanceof HTMLTextAreaElement ? field.value : (field.textContent ?? '');
}

describe('ChatComposer', () => {
  it('sends on Enter and clears the field', async () => {
    const onSend = vi.fn();
    render(<ChatComposer onSend={onSend} />);
    const field = messageField();
    await userEvent.type(field, 'Rebuild it{Enter}');
    expect(onSend).toHaveBeenCalledWith('Rebuild it');
    expect(fieldText(field).trim()).toBe('');
  });

  it('breaks the line on Shift+Enter instead of sending', async () => {
    const onSend = vi.fn();
    render(<ChatComposer onSend={onSend} />);
    await userEvent.type(messageField(), 'one{Shift>}{Enter}{/Shift}two');
    expect(onSend).not.toHaveBeenCalled();
    /*
     * The exact string, because everything looser passed on a Shift+Enter that
     * inserted *nothing*: `.replace(/\n/g, '\n')` is the identity function,
     * `toContain('one')` is true of any field that took the letters, and the
     * `\s*` in `/one\s*two/` matches the empty string — so `"onetwo"`, the
     * precise failure this test names, satisfied all three.
     *
     * The break is normalised first: a contenteditable may serialise one line
     * break as `\n`, as `\r\n`, or (with a trailing `<br>` filler) with a
     * second `\n` after it, and none of those differences are this test's
     * subject. What is its subject — that there is exactly one break, with
     * `one` before it and `two` after it — survives the normalisation.
     */
    const written = fieldText(messageField()).replace(/\r\n/g, '\n').replace(/\n+$/, '');
    expect(written).toBe('one\ntwo');
  });

  /*
   * Enter belongs to the input method while it is composing.
   *
   * Reproduced in a real browser before the fix, with `Input.imeSetComposition`:
   * typing `ceshi` and pressing Enter to accept 测试 sent the literal pinyin as
   * a turn. In the live app the composition then commits into the box that was
   * just cleared, which is what "sending doesn't clear the box" looks like from
   * the outside. Everyone typing Chinese, Japanese or Korean hits this on their
   * first message.
   */
  it('leaves Enter to the input method while it is composing', async () => {
    const onSend = vi.fn();
    render(<ChatComposer onSend={onSend} />);
    const field = messageField();
    await userEvent.type(field, 'ceshi');

    fireEvent.keyDown(field, { key: 'Enter', isComposing: true });
    expect(onSend).not.toHaveBeenCalled();

    fireEvent.keyDown(field, { key: 'Enter' });
    expect(onSend).toHaveBeenCalledWith('ceshi');
  });

  it('sends from the button as well as the key', async () => {
    const onSend = vi.fn();
    render(<ChatComposer onSend={onSend} />);
    await userEvent.type(messageField(), 'Ship it');
    await userEvent.click(screen.getByRole('button', { name: 'Send' }));
    expect(onSend).toHaveBeenCalledWith('Ship it');
  });

  /*
   * The unavailability is the assertion, not a precondition for one.
   *
   * This stood as a `does not send %s` with an early return: "if Send is
   * unavailable, expect `onSend` not called and stop". Once `canSend` was
   * restored *both* rows took that branch, so neither ever pressed anything and
   * the body was `expect(a fresh mock).not.toHaveBeenCalled()` — true of any
   * implementation, including one that sends whitespace happily the moment the
   * button is enabled again. What the composer actually promises is that a
   * draft with no words in it leaves Send unavailable, so that is what is read.
   */
  it.each([['blank', ''], ['only whitespace', '   ']])(
    'marks Send unavailable on a %s draft and sends nothing when it is pressed',
    async (_label, text) => {
      const onSend = vi.fn();
      render(<ChatComposer onSend={onSend} />);
      if (text !== '') await userEvent.type(messageField(), text);
      const send = screen.getByRole('button', { name: 'Send' });
      expect(send.hasAttribute('disabled') || send.getAttribute('aria-disabled') === 'true')
        .toBe(true);
      await userEvent.click(send);
      expect(onSend).not.toHaveBeenCalled();
    },
  );

  /*
   * ── §5.1, restored with the constraint it was supposed to carry ──────────
   *
   * What stood here was `expect(getByRole('button', { name: 'Send' }))
   * .toBeTruthy()` — a tautology, since `getByRole` throws when it finds
   * nothing, so the assertion could not fail on any tree the line above it
   * survived. It replaced §5.1's `marks Send unusable without taking it out of
   * the focus order`, and it kept none of that test's force: it passed
   * unchanged against the bug it was standing in for, a Send that reported
   * `{ disabled: false, ariaDisabled: null }` over an empty field and did
   * nothing when pressed.
   *
   * Two claims, split apart because they fail for different reasons.
   */
  it('marks Send unavailable over an empty field instead of looking pressable', async () => {
    const onSend = vi.fn();
    render(<ChatComposer onSend={onSend} />);
    const send = screen.getByRole('button', { name: 'Send' });
    /* Either vocabulary is honest; *neither* is the bug. Astryx picks native
       `disabled` here because `ChatSendButton` takes no tooltip — see the
       `sendButton` note in `public.tsx` for why that is the available choice
       and what it costs. */
    expect(send.hasAttribute('disabled') || send.getAttribute('aria-disabled') === 'true').toBe(true);
    await userEvent.click(send);
    expect(onSend).not.toHaveBeenCalled();

    /* And it comes back the moment there is something to send, so "unavailable"
       is a state and not a permanent condition. */
    await userEvent.type(messageField(), 'Ship it');
    const live = screen.getByRole('button', { name: 'Send' });
    expect(live.hasAttribute('disabled')).toBe(false);
    expect(live.getAttribute('aria-disabled')).not.toBe('true');
  });

  /*
   * The other half of §5.1, and the reason a natively disabled Send is
   * survivable here: sending from the button empties the draft, which makes the
   * button that was just clicked unavailable *under the user's own focus*. A
   * natively disabled element cannot hold focus, so without somewhere to put it
   * the document hands it to `<body>` and the next Tab restarts from the top.
   *
   * **What this tier can and cannot say.** It renders a composer with no
   * `disabled` prop, and the app never builds one: both router call sites pass
   * `disabled={store.sending}`, and `send()` flips that flag synchronously, so
   * in production the field is `contenteditable="false"` by the time the restore
   * runs. jsdom would not notice either — it does not drop focus off a
   * `contenteditable` going false. So this pins the plain case only; the case
   * the app actually runs is in `thread.browser.test.tsx`.
   */
  it('leaves focus in the field, never on <body>, when Send goes away under it', async () => {
    render(<ChatComposer onSend={vi.fn()} />);
    const field = messageField();
    await userEvent.type(field, 'Ship it');
    const send = screen.getByRole('button', { name: 'Send' });
    send.focus();
    expect(document.activeElement).toBe(send);

    await userEvent.click(send);

    expect(document.activeElement).not.toBe(document.body);
    expect(document.activeElement).toBe(messageField());
  });

  it('turns Send into Stop while a turn is running', async () => {
    const onStop = vi.fn();
    render(<ChatComposer onSend={vi.fn()} onStop={onStop} />);
    const stop = screen.getByRole('button', { name: 'Stop' });
    expect(screen.queryByRole('button', { name: 'Send' })).toBeNull();
    await userEvent.click(stop);
    expect(onStop).toHaveBeenCalledOnce();
  });

  /*
   * A second press reaches the callback, and that is the honest arrangement.
   *
   * The composer briefly withheld `onStop` after the first press, to say "a stop
   * already asked for cannot be asked for again". Astryx's Stop is enabled
   * whenever it is shown (`isDisabled={!isStopShown && isDisabled}`), so
   * withholding the callback changed nothing about how the button looks or
   * announces itself — it only emptied its `onClick`, which is the "looks
   * pressable, does nothing" shape the file's own note forbids. The refusal
   * belongs where the state that decides it lives, at the top of the router's
   * `interrupt()`; here Stop stays a button that reports what it did.
   */
  it('keeps Stop live and lets a second press through to the caller', async () => {
    const onStop = vi.fn();
    render(<ChatComposer onSend={vi.fn()} onStop={onStop} />);
    const stop = screen.getByRole('button', { name: 'Stop' });
    await userEvent.click(stop);
    /* Still shown, still pressable — nothing about the first press changed it. */
    expect(screen.getByRole('button', { name: 'Stop' })).toBe(stop);
    expect(stop.hasAttribute('disabled')).toBe(false);
    expect(stop.getAttribute('aria-disabled')).not.toBe('true');
    await userEvent.click(stop);
    expect(onStop).toHaveBeenCalledTimes(2);
  });
});
