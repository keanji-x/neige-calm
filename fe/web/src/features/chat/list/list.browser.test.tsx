/* The two-part conversation row (name + track crumb), measured in a real rendering engine. */
import { render } from '@testing-library/react';
import { page as browserPage } from 'vitest/browser';
import { afterEach, describe, expect, it } from 'vitest';

/* The whole cascade before the CSS Module: whichever module declares `@layer features` first registers it, and every app override then loses. */
import '../../../styles/entry.css';

import {
  CONVERSATION_NAME_MAX, type Conversation,
} from '../../../../../core/domain/conversation.ts';
import { ChatList } from './public.tsx';

afterEach(() => { document.body.replaceChildren(); });

/* The panel track is `max(15rem, 25cqi)`: 240px at its floor, ~280px on a wide window. */
const COLUMN = 270;

const COLUMN_FLOOR = 240;

const LONG_TRACK = 'Ship the conversation panel rewrite and its migration plan';

const LONGEST_NAME = 'Rework how the track panel lists conversations'
  .padEnd(CONVERSATION_NAME_MAX, '!');

function conversation(overrides: Partial<Conversation> = {}): Conversation {
  return {
    id: 'c1', trackId: 'w1', trackTitle: LONG_TRACK, title: 'Assistant',
    kind: 'track-assistant', state: null, updatedAt: 1, ...overrides,
  };
}

function draw(conversations: readonly Conversation[], width = COLUMN) {
  const { getByRole } = render(
    <div style={{ inlineSize: width }}>
      <ChatList conversations={conversations} cards={{}} onOpen={() => undefined} />
    </div>,
  );
  const row = getByRole('button');
  const label = row.firstElementChild as HTMLElement;
  const parts = [...label.children] as HTMLElement[];
  expect(parts, 'the row is no longer `label > (name, track)`').toHaveLength(2);
  const [name, track] = parts as [HTMLElement, HTMLElement];
  const [only] = conversations as readonly [Conversation];
  expect(name.textContent, 'the first child is not the conversation name').toBe(only.title);
  expect(track.textContent, 'the second child is not the track crumb').toBe(only.trackTitle);
  return { row, label, name, track };
}

const clipped = (element: HTMLElement) => element.scrollWidth > element.clientWidth + 1;

describe('a conversation row that names its track', () => {
  it('keeps a short name whole however long the track title is', async () => {
    await browserPage.viewport(1200, 800);
    const { row, label, name, track } = draw([conversation()]);

    expect(clipped(track), 'the track title fits, so nothing is being shared')
      .toBe(true);

    expect(clipped(name), 'the conversation name was truncated').toBe(false);

    expect(row.scrollWidth).toBeLessThanOrEqual(row.clientWidth + 1);
    expect(label.scrollWidth).toBeLessThanOrEqual(label.clientWidth + 1);
  });

  it('gives the name the majority of the line when neither half fits', async () => {
    await browserPage.viewport(1200, 800);
    const { row, label, name, track } = draw([conversation({
      title: 'Rename this conversation',
    })]);

    expect(clipped(name), 'the premise: this name cannot fit either').toBe(true);
    expect(clipped(track), 'the premise: nor can this crumb').toBe(true);
    /* 40% is the crumb's ceiling, so the name gets the majority. */
    expect(name.clientWidth / label.clientWidth).toBeGreaterThan(0.5);
    expect(row.scrollWidth).toBeLessThanOrEqual(row.clientWidth + 1);
    expect(label.scrollWidth).toBeLessThanOrEqual(label.clientWidth + 1);
  });

  it('never leaves the separator standing where the track title does not fit', async () => {
    await browserPage.viewport(1200, 800);
    const { row, label, name, track } = draw([conversation({
      title: LONGEST_NAME, trackTitle: 'Ops',
    })], COLUMN_FLOOR);

    expect(clipped(name), 'the premise: the name has to give').toBe(true);
    expect(clipped(track), 'the track title was cut down to its separator').toBe(false);
    expect(row.scrollWidth).toBeLessThanOrEqual(row.clientWidth + 1);
    expect(label.scrollWidth).toBeLessThanOrEqual(label.clientWidth + 1);
  });
});
