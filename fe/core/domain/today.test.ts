import { describe, expect, it } from 'vitest';

import type { ApiFailure } from '../api/types.js';
import {
  nameTodaySummaryConversation, NOTHING_TO_SUMMARISE, TODAY_SUMMARY_CONVERSATION_KEY,
  TODAY_SUMMARY_CONVERSATION_TITLE, todayLaunchpadOperation, todaySummaryFailure, todaySummaryOperation,
} from './today.js';
import { trackConversationCardId, type Conversation } from './conversation.js';

const http = (status: number, code: string, message: string): ApiFailure =>
  ({ kind: 'http', status, code, message });

describe('the Today summary trigger (#1253 D5)', () => {
  /*
   * The endpoint takes no request body, and this pins the absence.
   *
   * It is the one property of this operation worth a test: the whole prompt is
   * synthesised server-side from an activity projection the frontend has no
   * read for, so a `body` appearing here would mean someone had started
   * sending one — which is the layer the design deleted, growing back on the
   * client side.
   */
  it('sends no prompt and no body at all', () => {
    const operation = todaySummaryOperation();
    expect(operation.method).toBe('POST');
    expect(operation.path).toBe('/api/today/summary');
    expect(operation.body).toBeUndefined();
  });

  it('decodes the track and the conversation card the server answers with', () => {
    const parsed = todaySummaryOperation().responseSchema.parse({ track_id: 'lp', card_id: 'conv-1' });
    expect(parsed).toEqual({ track_id: 'lp', card_id: 'conv-1' });
  });

  /* The read the page load uses is a GET, and it stays one. Stated here beside
     the trigger because the pair is the whole contract: exactly one of these
     two writes, and it is never the one on the render path. */
  it('leaves the page-load resolve a pure read', () => {
    expect(todayLaunchpadOperation().method).toBe('GET');
  });

  it('names the fixed summary writer without exposing its bootstrap instruction', () => {
    const summary: Conversation = {
      id: trackConversationCardId('lp', TODAY_SUMMARY_CONVERSATION_KEY),
      trackId: 'lp', title: null, kind: 'track-assistant', state: 'idle', updatedAt: 1,
    };
    expect(nameTodaySummaryConversation('lp', summary).title).toBe(TODAY_SUMMARY_CONVERSATION_TITLE);
    expect(nameTodaySummaryConversation('lp', { ...summary, title: 'Server title' }).title).toBe('Server title');
    expect(nameTodaySummaryConversation('other-track', summary)).toBe(summary);
  });
});

describe('todaySummaryFailure', () => {
  /*
   * The refusal that is not a malfunction.
   *
   * The server answers 409 for three different things — the generic `conflict`
   * from the underlying conversation create, `planner_harness_dormant` from a send
   * that could not be recovered, and this one — so the status cannot be what
   * distinguishes them. The three cases below are one status and three answers.
   */
  it('reads an empty day as a fact about the day', () => {
    expect(todaySummaryFailure(http(409, 'today_summary_no_activity', 'nothing happened'))).toEqual({
      kind: 'no-activity', message: NOTHING_TO_SUMMARISE,
    });
  });

  it('does not mistake the other 409s for an empty day', () => {
    expect(todaySummaryFailure(http(409, 'conflict', 'already exists')).kind).toBe('error');
    expect(todaySummaryFailure(http(409, 'planner_harness_dormant', 'dormant')).kind).toBe('error');
  });

  /* Matching on the sentence rather than the code would be mirror code for a
     string the server owns — and it would misread this one, which contains the
     words but carries a different code. */
  it('matches the code and not the message text', () => {
    expect(todaySummaryFailure(http(500, 'internal', 'today_summary_no_activity')).kind).toBe('error');
  });

  it('separates "the agent service is down" from "something went wrong"', () => {
    const unavailable = todaySummaryFailure(http(503, 'service_unavailable', 'app-server not running'));
    expect(unavailable.kind).toBe('unavailable');
    expect(unavailable.message).toContain('app-server not running');
  });

  it('keeps the server sentence for anything it cannot classify', () => {
    expect(todaySummaryFailure(http(500, 'internal', 'it exploded'))).toEqual({
      kind: 'error', message: 'it exploded',
    });
    expect(todaySummaryFailure({ kind: 'transport', message: 'offline' })).toEqual({
      kind: 'error', message: 'offline',
    });
  });
});
