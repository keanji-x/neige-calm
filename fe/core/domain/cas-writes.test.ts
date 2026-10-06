// #2131 S5: the recipe and calendar write tables, the one reading of a failed write, and a CAS writer's read-back of a
// stale answer to a retry whose previous outcome was unknown.
import { describe, expect, it, vi } from 'vitest';

import type { ApiFailure } from '../api/types.js';
import {
  CALENDAR_WRITE_FAILURES, CALENDAR_WRITE_TEXT, calendarReadBackWindow, calendarUpdateLanded, calendarWriteFailureText,
  type CalendarListedEntry, type CalendarUpdate,
} from './calendar.js';
import { ApiError, casAttempts, classifyFailure, NotSentError, readWriteFailure, type FailureTable } from './failure-class.js';
import { RECIPE_CREATE_FAILURES, RECIPE_SAVE_FAILURES, RECIPE_SAVE_TEXT, recipeSaveLanded, type TrackRecipe } from './track.js';

const http = (status: number, code = 'http_error', message = 'answered'): ApiFailure =>
  ({ kind: 'http', status, code, message, body: { error: message, code } });
const unauthorized: ApiFailure = { kind: 'unauthorized', status: 401, code: 'unauthorized', message: 'signed out' };
const transport: ApiFailure = { kind: 'transport', message: 'dropped' };
const decode: ApiFailure = { kind: 'decode', message: 'malformed' };

/** Each table against every failure kind: 401, each status and code the server answers, a lost answer, an unreadable one, none. */
const cases: ReadonlyArray<readonly [string, FailureTable<string>, ReadonlyArray<readonly [ApiFailure | null, string]>]> = [
  ['POST /track-recipes', RECIPE_CREATE_FAILURES, [
    [http(400, 'bad_request'), 'refused'], [http(403, 'forbidden'), 'refused'], [http(413), 'refused'], [http(422), 'refused'],
    [unauthorized, 'refused'],
    /* Final for the key: a create that failed for good, and one that stopped part way and may have made the recipe. */
    [http(409, 'idempotency_key_reused'), 'refused'], [http(500, 'operation_failed'), 'refused'],
    [http(500, 'operation_stuck'), 'stuck'],
    /* No key yet: anything else may have made the recipe. */
    [http(409, 'conflict'), 'unknown'], [http(500, 'db_error'), 'unknown'], [http(503), 'unknown'],
    [transport, 'unknown'], [decode, 'unknown'], [null, 'unknown'],
  ]],
  ['PUT /track-recipes/{id}', RECIPE_SAVE_FAILURES, [
    /* The `if_revision` CAS lost. */
    [http(409, 'conflict'), 'stale'],
    [http(400, 'bad_request'), 'refused'], [http(403, 'forbidden'), 'refused'], [http(404, 'not_found'), 'refused'],
    [http(413), 'refused'], [http(422), 'refused'], [unauthorized, 'refused'],
    [http(500, 'db_error'), 'unknown'], [http(503), 'unknown'],
    [transport, 'unknown'], [decode, 'unknown'], [null, 'unknown'],
  ]],
  ['POST /calendar/tasks', CALENDAR_WRITE_FAILURES.create, [
    /* A key bound to other content: final for that key. */
    [http(409, 'conflict'), 'refused'],
    [http(400, 'bad_request'), 'refused'], [http(403, 'forbidden'), 'refused'], [http(413), 'refused'], [http(422), 'refused'],
    [http(503, 'service_unavailable'), 'refused'], [unauthorized, 'refused'],
    [http(500, 'internal'), 'unknown'], [http(502), 'unknown'],
    [transport, 'unknown'], [decode, 'unknown'], [null, 'unknown'],
  ]],
  ['POST /calendar/tasks/{id}', CALENDAR_WRITE_FAILURES.update, [
    /* The `expected_version` CAS lost. */
    [http(409, 'conflict'), 'stale'],
    [http(400, 'bad_request'), 'refused'], [http(403, 'forbidden'), 'refused'], [http(404, 'not_found'), 'refused'],
    [http(413), 'refused'], [http(422), 'refused'], [http(503, 'service_unavailable'), 'refused'], [unauthorized, 'refused'],
    [http(500, 'internal'), 'unknown'], [http(502), 'unknown'],
    [transport, 'unknown'], [decode, 'unknown'], [null, 'unknown'],
  ]],
];

describe.each(cases)('classifying a failed %s', (_route, table, expected) => {
  it.each(expected)('reads %o as %s', (failure, kind) => {
    expect(classifyFailure(failure, table)).toBe(kind);
  });
});

describe('readWriteFailure', () => {
  it('reads a stale answer with no sentence, a refusal in the server’s words and an unknown outcome in the fixed one', () => {
    expect(readWriteFailure(new ApiError(http(409, 'conflict', 'revision 8')), RECIPE_SAVE_FAILURES, RECIPE_SAVE_TEXT)).toEqual({ is: 'stale' });
    expect(readWriteFailure(new ApiError(http(400, 'bad_request', 'unclosed fence')), RECIPE_SAVE_FAILURES, RECIPE_SAVE_TEXT))
      .toEqual({ is: 'refused', text: 'unclosed fence' });
    expect(readWriteFailure(new ApiError(transport), RECIPE_SAVE_FAILURES, RECIPE_SAVE_TEXT)).toEqual({ is: 'unknown', text: RECIPE_SAVE_TEXT.unknown });
  });

  it('reads a write that was not sent as refused in the fixed words, never as stale or unknown', () => {
    expect(readWriteFailure(new NotSentError(), RECIPE_SAVE_FAILURES, RECIPE_SAVE_TEXT)).toEqual({ is: 'refused', text: RECIPE_SAVE_TEXT.refused });
  });
});

describe('calendarWriteFailureText', () => {
  const update: CalendarUpdate = { id: 'one', expected_version: 3, task: { title: 'A', description: '', schedule: { kind: 'all_day', date: '2026-10-02' } }, cancelled: false };

  it('says each op’s own fixed sentence and the stale sentence, and never the runner’s offline words', () => {
    const lost = new ApiError(transport);
    expect(calendarWriteFailureText({ task: update.task })(lost)).toBe(CALENDAR_WRITE_TEXT.create.unknown);
    expect(calendarWriteFailureText(update)(lost)).toBe(CALENDAR_WRITE_TEXT.update.unknown);
    expect(calendarWriteFailureText({ ...update, cancelled: true })(lost)).toBe(CALENDAR_WRITE_TEXT.cancel.unknown);
    expect(calendarWriteFailureText(update)(new ApiError(http(409, 'conflict', 'changed')))).toBe(CALENDAR_WRITE_TEXT.stale);
    expect(calendarWriteFailureText({ task: update.task })(new NotSentError())).toBe(CALENDAR_WRITE_TEXT.create.refused);
  });
});

describe('casAttempts', () => {
  const stale = () => Promise.reject(new ApiError(http(409, 'conflict')));
  const lost = () => Promise.reject(new ApiError(transport));
  const attempt = { id: 'r', body: 'text', if_revision: 7 };

  it('reads back a stale answer to a retry of an unknown attempt, and confirms it with what holds', async () => {
    const attempts = casAttempts(RECIPE_SAVE_FAILURES);
    const landed = vi.fn(() => Promise.resolve({ stored: 'stored row' }));
    await expect(attempts(attempt, lost, landed)).rejects.toBeInstanceOf(ApiError);
    await expect(attempts({ ...attempt }, stale, landed)).resolves.toBe('stored row');
    expect(landed).toHaveBeenCalledTimes(1);
  });

  it('keeps the stale answer when what is stored is not this attempt', async () => {
    const attempts = casAttempts(RECIPE_SAVE_FAILURES);
    await expect(attempts(attempt, lost, () => Promise.resolve(null))).rejects.toBeInstanceOf(ApiError);
    const error = await attempts(attempt, stale, () => Promise.resolve(null)).catch((reason: unknown) => reason);
    expect(readWriteFailure(error, RECIPE_SAVE_FAILURES, RECIPE_SAVE_TEXT).is).toBe('stale');
  });

  it('never reads back a stale answer to a first attempt or a changed attempt', async () => {
    const landed = vi.fn(() => Promise.resolve({ stored: 'stored row' }));
    const first = casAttempts(RECIPE_SAVE_FAILURES);
    await expect(first(attempt, stale, landed)).rejects.toBeInstanceOf(ApiError);
    const changed = casAttempts(RECIPE_SAVE_FAILURES);
    await expect(changed(attempt, lost, landed)).rejects.toBeInstanceOf(ApiError);
    await expect(changed({ ...attempt, body: 'edited' }, stale, landed)).rejects.toBeInstanceOf(ApiError);
    expect(landed).not.toHaveBeenCalled();
  });

  /* A refusal of a retry says nothing about whether the earlier unknown attempt landed (#2166 N1). */
  it.each([
    { write: 'a recipe save', status: 401, table: RECIPE_SAVE_FAILURES, refusal: unauthorized },
    { write: 'a recipe save', status: 400, table: RECIPE_SAVE_FAILURES, refusal: http(400) },
    { write: 'a calendar update', status: 503, table: CALENDAR_WRITE_FAILURES.update, refusal: http(503, 'service_unavailable') },
  ])(
    'still reads back $write after a $status refusal of the same attempt that followed an unknown one', async ({ table, refusal }) => {
      const attempts = casAttempts(table);
      await expect(attempts(attempt, lost, () => Promise.resolve(null))).rejects.toBeInstanceOf(ApiError);
      await expect(attempts(attempt, () => Promise.reject(new ApiError(refusal)), () => Promise.resolve(null))).rejects.toBeInstanceOf(ApiError);
      await expect(attempts(attempt, stale, () => Promise.resolve({ stored: 'stored row' }))).resolves.toBe('stored row');
    },
  );

  it('forgets an unknown attempt once a changed attempt, a success or a stale answer settles it', async () => {
    const landed = vi.fn(() => Promise.resolve({ stored: 'stored row' }));
    const changed = casAttempts(RECIPE_SAVE_FAILURES);
    await expect(changed(attempt, lost, landed)).rejects.toBeInstanceOf(ApiError);
    await expect(changed({ ...attempt, body: 'edited' }, () => Promise.reject(new ApiError(http(400))), landed)).rejects.toBeInstanceOf(ApiError);
    await expect(changed(attempt, stale, landed)).rejects.toBeInstanceOf(ApiError);
    const succeeded = casAttempts(RECIPE_SAVE_FAILURES);
    await expect(succeeded(attempt, lost, landed)).rejects.toBeInstanceOf(ApiError);
    await expect(succeeded(attempt, () => Promise.resolve('saved'), landed)).resolves.toBe('saved');
    await expect(succeeded(attempt, stale, landed)).rejects.toBeInstanceOf(ApiError);
    const concluded = casAttempts(RECIPE_SAVE_FAILURES);
    await expect(concluded(attempt, lost, landed)).rejects.toBeInstanceOf(ApiError);
    await expect(concluded(attempt, stale, () => Promise.resolve(null))).rejects.toBeInstanceOf(ApiError);
    await expect(concluded(attempt, stale, landed)).rejects.toBeInstanceOf(ApiError);
    expect(landed).not.toHaveBeenCalled();
  });

  it('leaves a retry unknown when its read-back fails, and reads back again on the next retry', async () => {
    const attempts = casAttempts(RECIPE_SAVE_FAILURES);
    await expect(attempts(attempt, lost, () => Promise.resolve(null))).rejects.toBeInstanceOf(ApiError);
    const error = await attempts(attempt, stale, () => Promise.reject(new ApiError(transport))).catch((reason: unknown) => reason);
    expect(readWriteFailure(error, RECIPE_SAVE_FAILURES, RECIPE_SAVE_TEXT)).toEqual({ is: 'unknown', text: RECIPE_SAVE_TEXT.unknown });
    await expect(attempts(attempt, stale, () => Promise.resolve({ stored: 'stored row' }))).resolves.toBe('stored row');
  });
});

describe('what a read-back holds', () => {
  const recipe: TrackRecipe = { id: 'r', title: 'Ship', body: 'Saved.', revision: 8, created_at: 1, updated_at: 2 };

  it('finds a recipe save only when the stored row holds exactly the title and body it sent', () => {
    expect(recipeSaveLanded(recipe, { title: 'Ship', body: 'Saved.' })).toEqual({ stored: recipe });
    expect(recipeSaveLanded(recipe, { title: 'Ship', body: 'Other.' })).toBeNull();
    expect(recipeSaveLanded(recipe, { title: 'Shipped', body: 'Saved.' })).toBeNull();
  });

  const timed = { kind: 'timed' as const, start: '2026-10-02T14:00:00+08:00', end: '2026-10-02T15:00:00+08:00', timezone: 'Asia/Shanghai' };
  const write: CalendarUpdate = { id: 'one', expected_version: 3, task: { title: 'A', description: 'd', schedule: timed }, cancelled: false };
  const listed = (task: CalendarUpdate['task']): CalendarListedEntry => ({
    id: 'one', task, version: 4, cancelled: false, source_track_id: null, created_by: 'user', created_at: 1, updated_at: 2, occurrences: [],
  });

  it('finds a calendar edit when the listed task is the one sent, whatever its key order', () => {
    const reordered = { schedule: { timezone: timed.timezone, end: timed.end, start: timed.start, kind: timed.kind }, description: 'd', title: 'A' };
    expect(calendarUpdateLanded([listed(reordered)], write)).toEqual({ stored: undefined });
    expect(calendarUpdateLanded([listed({ ...write.task, title: 'B' })], write)).toBeNull();
    expect(calendarUpdateLanded([], write)).toBeNull();
  });

  it('finds a calendar cancel when the task is no longer listed around its schedule', () => {
    expect(calendarUpdateLanded([], { ...write, cancelled: true })).toEqual({ stored: undefined });
    expect(calendarUpdateLanded([listed(write.task)], { ...write, cancelled: true })).toBeNull();
  });

  it('reads back through the widest window the server lists, centred on the sent schedule', () => {
    expect(calendarReadBackWindow(write.task)).toEqual({ from: '2026-04-02', until: '2027-04-03', timezone: 'Asia/Shanghai' });
    expect(calendarReadBackWindow({ ...write.task, schedule: { kind: 'all_day', date: '2026-10-02' } }))
      .toEqual({ from: '2026-04-02', until: '2027-04-03', timezone: 'UTC' });
  });
});
