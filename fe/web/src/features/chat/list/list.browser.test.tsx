/*
 * The two-part row, against a real rendering engine.
 *
 * A row on Today says two things on one line — the conversation's own name and,
 * quieter, the wave it lives on — and the whole reason it says the first one is
 * that a wave now contributes *several* rows to that list. Rows that all read
 * `Test wave` are rows a sighted reader cannot choose between (#1189 §5).
 *
 * **Only an engine can answer whether that is still true when the crumb is
 * long.** `display: flex`, `overflow`, `text-overflow` and `max-inline-size`
 * are strings jsdom stores and never applies: the `textContent` assertions in
 * `app/router/wave-conversation.test.tsx` ([G5]) are green whatever these rules
 * say, because both strings are in the DOM either way and jsdom will not tell
 * you that one of them is five characters wide. The neighbouring
 * `features/wave/page/panel-sticky.browser.test.tsx` records the same lesson
 * about the pair `max-block-size` / `overflow-y`, which is how the panel's
 * eight-row cap went untested for as long as it did.
 *
 * The defect this pins is not hypothetical and is not a wrong declaration: two
 * items that are both `flex: 0 1 auto` shrink *in proportion to their base
 * size*, so a 60-character wave title takes nearly the whole line and leaves
 * the name a stub. Every rule involved reads correctly in the stylesheet. The
 * only way to see it is to measure.
 */
import { render } from '@testing-library/react';
import { page as browserPage } from 'vitest/browser';
import { afterEach, describe, expect, it } from 'vitest';

/* The whole cascade, and before the CSS Module — see the long note on import
   order in `features/chat/thread/thread.browser.test.tsx`. A module that
   declares `@layer features` of its own registers that layer first if it is
   imported first, and every override in the app then loses. */
import '../../../styles/entry.css';

import {
  CONVERSATION_NAME_MAX, type Conversation,
} from '../../../../../core/domain/conversation.ts';
import { ChatList } from './public.tsx';

afterEach(() => { document.body.replaceChildren(); });

/* The conversation module's real column. The panel track is `--panel-span`,
   `max(15rem, 25cqi)` (`app/shell/shell.module.css`): 240px at its floor and
   around 280px on a wide window, so 270 is a width this panel really takes.
   A probe run at viewport width would be measuring a row this app never
   draws — the defect is entirely about how little line there is to share. */
const COLUMN = 270;

/* The floor of that same track: `max(15rem, 25cqi)` is 240px on a narrow
   window, and it is where the two halves are most contended. */
const COLUMN_FLOOR = 240;

const LONG_WAVE = 'Ship the conversation panel rewrite and its migration plan';

/* A derived name is clipped to `CONVERSATION_NAME_MAX`, so this is the longest
   one a row can carry, and the worst case for everything to its right. Padded
   from the constant rather than typed out: a name one character short of the
   bound would be testing a case the product can beat. */
const LONGEST_NAME = 'Rework how the wave panel lists conversations'
  .padEnd(CONVERSATION_NAME_MAX, '!');

function conversation(overrides: Partial<Conversation> = {}): Conversation {
  return {
    id: 'c1', waveId: 'w1', waveTitle: LONG_WAVE, title: 'Assistant',
    kind: 'wave-assistant', state: null, updatedAt: 1, ...overrides,
  };
}

/**
 * The real `ChatList`, in a column the width the panel gives it.
 *
 * The two boxes are reached **by position**, not by class: a CSS Module class
 * is a hashed runtime value and `architecture/no-class-dom-query` closes that
 * door on purpose, and adding `data-testid`s to a production row to measure it
 * would be putting test scaffolding in the product. The shape is `button >
 * span.label > (span.name, span.wave)` and it is one component away — a change
 * to it fails here loudly rather than silently, which is the right direction.
 *
 * "Loudly" is what the assertions below buy: the position assumption is checked
 * against the fixture's own two strings before anything is measured. `::before`
 * content is not a child, so today the arithmetic is right by accident as much
 * as by design — put a badge between the name and the crumb and every geometric
 * assertion in this file would go on passing while measuring the badge, with
 * the crumb an unexamined third child. A count and two `textContent`s cost one
 * line and turn that into a failure at the point of the change.
 */
function draw(conversations: readonly Conversation[], width = COLUMN) {
  const { getByRole } = render(
    <div style={{ inlineSize: width }}>
      <ChatList conversations={conversations} onOpen={() => undefined} />
    </div>,
  );
  const row = getByRole('button');
  const label = row.firstElementChild as HTMLElement;
  const parts = [...label.children] as HTMLElement[];
  expect(parts, 'the row is no longer `label > (name, wave)`').toHaveLength(2);
  const [name, wave] = parts as [HTMLElement, HTMLElement];
  const [only] = conversations as readonly [Conversation];
  expect(name.textContent, 'the first child is not the conversation name').toBe(only.title);
  expect(wave.textContent, 'the second child is not the wave crumb').toBe(only.waveTitle);
  return { row, label, name, wave };
}

/** Truncated, in the only sense the reader cares about: text is being hidden. */
const clipped = (element: HTMLElement) => element.scrollWidth > element.clientWidth + 1;

describe('a conversation row that names its wave', () => {
  it('keeps a short name whole however long the wave title is', async () => {
    await browserPage.viewport(1200, 800);
    const { row, label, name, wave } = draw([conversation()]);

    /*
     * The premise: this really is the contended case. Without it every
     * assertion below is satisfied by a row with room to spare, which is the
     * shape the defect does *not* appear in — and a shorter crumb would make
     * this file green against the very CSS it was written to reject.
     */
    expect(clipped(wave), 'the wave title fits, so nothing is being shared')
      .toBe(true);

    /*
     * `Assistant` is the name every wave-assistant row carries until its first
     * message names it, and it needs ~60px of a ~250px line. It gets them:
     * `clientWidth >= scrollWidth` is the lower bound that matters, because it
     * is stated in terms of the text rather than a magic number. Against the
     * pre-fix CSS — both halves `flex: 0 1 auto`, shrinking in proportion to
     * their base size — this is where the name fell to about five characters
     * and every row under one long-titled wave read `Rena…` `Fix …` with the
     * same crumb behind it.
     */
    expect(clipped(name), 'the conversation name was truncated').toBe(false);

    /* Neither half escapes the row: this is one line, and it ellipses rather
       than pushing the list sideways. */
    expect(row.scrollWidth).toBeLessThanOrEqual(row.clientWidth + 1);
    expect(label.scrollWidth).toBeLessThanOrEqual(label.clientWidth + 1);
  });

  it('gives the name the majority of the line when neither half fits', async () => {
    await browserPage.viewport(1200, 800);
    /* Both want more than they can have, which is the case a bound decides and
       a shrink ratio does not: weighted shrinkage hands the *longer* string
       more of the line, so the longer the crumb the smaller the name — the
       defect gets worse exactly as the wave title gets more repetitive. */
    const { row, label, name, wave } = draw([conversation({
      title: 'Rename this conversation',
    })]);

    expect(clipped(name), 'the premise: this name cannot fit either').toBe(true);
    expect(clipped(wave), 'the premise: nor can this crumb').toBe(true);
    /* The explicit floor. 40% is the crumb's ceiling, so what is left for the
       name is the rest of the line less the gap — a majority, and by a margin
       that does not depend on which two strings these are. */
    expect(name.clientWidth / label.clientWidth).toBeGreaterThan(0.5);
    expect(row.scrollWidth).toBeLessThanOrEqual(row.clientWidth + 1);
    expect(label.scrollWidth).toBeLessThanOrEqual(label.clientWidth + 1);
  });

  /*
   * The crumb's separator, at the width where it is the only thing left of it.
   *
   * The `·` is generated content at the crumb's inline *start* and clipping
   * happens at the inline *end*, so the dot is the last part of the crumb to
   * go — the opposite of the order that would be harmless. Shrinkage weighted
   * by base size does the rest: the shorter the wave title, the less of the
   * squeeze it absorbs, so the case that fails is the small one. The longest
   * name a conversation can have, a one-word wave, the panel at its floor —
   * every value here is a real bound rather than a chosen bad number — and the
   * crumb is handed about eight pixels: a dot, alone, after a name. It reads as
   * a mark of truncation over a string that needed two more characters, and the
   * wave it was there to name is the part that vanished.
   */
  it('never leaves the separator standing where the wave title does not fit', async () => {
    await browserPage.viewport(1200, 800);
    const { row, label, name, wave } = draw([conversation({
      title: LONGEST_NAME, waveTitle: 'Ops',
    })], COLUMN_FLOOR);

    /* The premise: the line really is over-subscribed, so the crumb is being
       shrunk rather than merely fitting. */
    expect(clipped(name), 'the premise: the name has to give').toBe(true);
    /* And the crumb survives whole — the wave is named, not hinted at. */
    expect(clipped(wave), 'the wave title was cut down to its separator').toBe(false);
    expect(row.scrollWidth).toBeLessThanOrEqual(row.clientWidth + 1);
    expect(label.scrollWidth).toBeLessThanOrEqual(label.clientWidth + 1);
  });
});
