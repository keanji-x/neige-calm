// #2131 S5: the calendar's writes through the real adapter over a fake server. An update or cancel is a CAS write on
// `expected_version`; a create is keyed. Each failure is read through `CALENDAR_WRITE_FAILURES`, a lost answer shows its
// fixed sentence, and a retry answered 409 because the reader's own lost write landed is confirmed, not called stale.
import { QueryClient, QueryClientProvider, onlineManager } from '@tanstack/react-query';
import { cleanup, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it } from 'vitest';

import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import type { CalendarDraft, CalendarEntry } from '../../../../core/domain/calendar.ts';
import { TodayCalendarTasks } from './calendar.tsx';

const unauthorized = createUnauthorizedChannel({ enqueue: (task) => task() });
const PLUGIN = { id: 'calendar', version: '0.1.0', enabled: true, state: 'running', manifest_name: 'Calendar',
  has_config: false, can_uninstall: false, can_disable: false };
const ENTRY: CalendarEntry = { id: 'one', task: { title: 'Research', description: '', schedule: { kind: 'all_day', date: '2026-10-02' } },
  version: 3, cancelled: false, source_track_id: null, created_by: 'user', created_at: 1, updated_at: 1 };
const ok = (body: unknown): ApiTransportResponse => ({ status: 200, statusText: 'OK', body });
const stale: ApiTransportResponse = { status: 409, statusText: 'Conflict',
  body: { error: 'conflict: calendar task changed; reload before editing', code: 'conflict' } };
const lost = (): Promise<ApiTransportResponse> => Promise.reject(new Error('socket hang up'));
const RAW = /socket hang up|Transport request failed|timed out|schema|offline|connection|Nothing was sent|conflict:/i;
/** Where the read-back window around ENTRY's date starts. */
const READ_BACK_FROM = 'from=2026-04-02&';
const STALE_TEXT = 'This task changed somewhere else. Close it and open it again to edit the current version.';

type World = { entries: CalendarEntry[] };
type Write = (request: ApiRequest, world: World, attempt: number) => Promise<ApiTransportResponse>;

/** `TodayCalendarTasks` over a fake server: lists answer from `world`, and each write goes to `write` with its 1-based attempt. */
function renderCalendar(write: Write) {
  const world: World = { entries: [ENTRY] };
  const writes: ApiRequest[] = [];
  const reads: string[] = [];
  const transport: ApiTransportPort = { send(request) {
    if (request.method !== 'GET') { writes.push(request); return write(request, world, writes.length); }
    reads.push(request.path);
    if (request.path === '/api/plugins') return Promise.resolve(ok([PLUGIN]));
    return Promise.resolve(ok(world.entries.filter((entry) => !entry.cancelled).map((entry) => ({ ...entry, occurrences: [] }))));
  } };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(<QueryClientProvider client={client}>
    <TodayCalendarTasks date="2026-10-02" onDateChange={() => undefined} transport={transport} unauthorized={unauthorized}
      onSettings={() => undefined} onOpenTrack={() => undefined} />
  </QueryClientProvider>);
  return { world, writes, reads };
}

/** The stored entry after an update `request` landed on it. */
function landed(request: ApiRequest): CalendarEntry {
  const body = request.body as { task: CalendarDraft; cancelled: boolean; expected_version: number };
  return { ...ENTRY, task: body.task, cancelled: body.cancelled, version: body.expected_version + 1 };
}

async function openResearch() {
  await userEvent.click(await within(await screen.findByRole('region', { name: 'Selected day tasks' })).findByRole('button', { name: /Research/ }));
  return within(screen.getByRole('dialog'));
}

async function renameTo(title: string) {
  const dialog = await openResearch();
  await userEvent.clear(dialog.getByRole('textbox', { name: 'Task title' }));
  await userEvent.type(dialog.getByRole('textbox', { name: 'Task title' }), title);
  await userEvent.click(dialog.getByRole('button', { name: 'Save changes' }));
  return dialog;
}

/** The dialog's alert sentences; its empty live region is not one. */
async function alertText() {
  return waitFor(() => {
    const texts = within(screen.getByRole('dialog')).getAllByRole('alert').map((alert) => alert.textContent ?? '').filter((text) => text !== '');
    if (texts.length === 0) throw new Error('No alert is shown.');
    return texts.join(' | ');
  });
}

afterEach(() => { cleanup(); onlineManager.setOnline(true); });

describe('a calendar update whose answer was lost', () => {
  it('confirms a retried save whose 409 is its own first save, landed', async () => {
    const { writes, reads } = renderCalendar((request, world, attempt) => {
      if (attempt > 1) return Promise.resolve(stale);
      world.entries = [landed(request)];
      return lost();
    });
    const dialog = await renameTo('Research done');
    expect(await alertText()).toBe('Saving the task is unconfirmed. Save again to check.');

    await userEvent.click(dialog.getByRole('button', { name: 'Save changes' }));
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    expect(writes.map((request) => request.body)).toEqual([writes[0].body, writes[0].body]);
    /* Read back through the widest window the server lists, centred on the sent date. */
    expect(reads).toContain(`/api/calendar/tasks?${READ_BACK_FROM}until=2027-04-03&timezone=UTC`);
  });

  it('still calls a retried save stale when someone else changed the task', async () => {
    renderCalendar((_request, world, attempt) => {
      if (attempt > 1) return Promise.resolve(stale);
      world.entries = [{ ...ENTRY, task: { ...ENTRY.task, title: 'Renamed elsewhere' }, version: 4 }];
      return lost();
    });
    const dialog = await renameTo('Research done');
    expect(await alertText()).toBe('Saving the task is unconfirmed. Save again to check.');

    await userEvent.click(dialog.getByRole('button', { name: 'Save changes' }));
    await waitFor(async () => expect(await alertText()).toBe(STALE_TEXT));
    expect(dialog.getByRole('textbox', { name: 'Task title' })).toHaveProperty('value', 'Research done');
  });

  it('confirms a retried cancel whose 409 is its own first cancel, landed', async () => {
    renderCalendar((request, world, attempt) => {
      if (attempt > 1) return Promise.resolve(stale);
      world.entries = [landed(request)];
      return lost();
    });
    const dialog = await openResearch();
    await userEvent.click(dialog.getByRole('button', { name: 'Cancel task' }));
    expect(await alertText()).toBe('Cancelling the task is unconfirmed. Cancel it again to check.');

    await userEvent.click(dialog.getByRole('button', { name: 'Cancel task' }));
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
  });

  it('calls a first 409 stale without reading the task back', async () => {
    const { reads } = renderCalendar(() => Promise.resolve(stale));
    await renameTo('Research done');
    await waitFor(async () => expect(await alertText()).toBe(STALE_TEXT));
    /* The read-back window is the only one that starts six months before the task; the list reads never do. */
    expect(reads.filter((path) => path.includes(READ_BACK_FROM))).toEqual([]);
  });

  it('shows a refused update in the server’s words', async () => {
    renderCalendar(() => Promise.resolve({ status: 404, statusText: 'Not Found', body: { error: 'not found: calendar task', code: 'not_found' } }));
    await renameTo('Research done');
    expect(await alertText()).toBe('not found: calendar task');
  });
});

describe('a calendar create', () => {
  async function create(title: string) {
    await userEvent.click(await screen.findByRole('button', { name: 'New task' }));
    await userEvent.type(screen.getByRole('textbox', { name: 'Task title' }), title);
    await userEvent.click(screen.getByRole('button', { name: 'Create task' }));
  }

  it('shows a lost answer as its fixed state, reads the lists again, and retries under the same key', async () => {
    const { writes, reads } = renderCalendar((request, world, attempt) => {
      if (attempt === 1) return lost();
      const body = request.body as { task: CalendarDraft };
      world.entries = [...world.entries, { ...ENTRY, id: 'two', task: body.task, version: 1 }];
      return Promise.resolve(ok(world.entries[1]));
    });
    await screen.findByRole('button', { name: /Research/ });
    const before = reads.length;
    await create('Write it up');
    const text = await alertText();
    expect(text).toBe('Creating the task is unconfirmed. Create it again to check; it is not added twice.');
    expect(text).not.toMatch(RAW);
    /* A write that may have been stored is followed by a fresh read, failure or not. */
    await waitFor(() => expect(reads.length).toBeGreaterThan(before));

    await userEvent.click(screen.getByRole('button', { name: 'Create task' }));
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    const keys = writes.map((request) => (request.body as { idempotency_key: string }).idempotency_key);
    expect(keys).toHaveLength(2);
    expect(keys[1]).toBe(keys[0]);
  });

  it('releases the key once a create is made, so the same task created again is a new task', async () => {
    const { writes } = renderCalendar((request, world) => {
      const body = request.body as { task: CalendarDraft };
      const made = { ...ENTRY, id: `made-${world.entries.length}`, task: body.task, version: 1 };
      world.entries = [...world.entries, made];
      return Promise.resolve(ok(made));
    });
    await create('Write it up');
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    await create('Write it up');
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    const keys = writes.map((request) => (request.body as { idempotency_key: string }).idempotency_key);
    expect(keys).toHaveLength(2);
    expect(keys[1]).not.toBe(keys[0]);
  });

  it('shows a key bound to other content in the server’s words', async () => {
    renderCalendar(() => Promise.resolve({ status: 409, statusText: 'Conflict',
      body: { error: 'conflict: idempotency key already used for different content', code: 'conflict' } }));
    await create('Write it up');
    expect(await alertText()).toBe('conflict: idempotency key already used for different content');
  });

  it('refuses an offline create at the press in the table’s words, not the runner’s', async () => {
    const { writes } = renderCalendar(() => Promise.resolve(ok(ENTRY)));
    await screen.findByRole('button', { name: /Research/ });
    onlineManager.setOnline(false);
    await create('Write it up');
    expect(await alertText()).toBe('The task was not created.');
    expect(writes).toEqual([]);
  });
});
