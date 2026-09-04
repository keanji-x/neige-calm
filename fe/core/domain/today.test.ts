import { describe, expect, it } from 'vitest';

import {
  nameTodaySummaryConversation, TODAY_SUMMARY_CONVERSATION_KEY,
  TODAY_SUMMARY_CONVERSATION_TITLE, todayLaunchpadEnsureOperation,
  todayLaunchpadOperation, todayReportResetOperation,
} from './today.js';
import { trackConversationCardId, type Conversation } from './conversation.js';

describe('the Today report reset (#1343)', () => {
  /*
   * The endpoint takes no request body, and this pins the absence.
   *
   * It is the one property of this operation worth a test. The canonical empty
   * document is ~2.6 kB of kernel-owned text and the empty-state predicate
   * compares it byte for byte, so a `body` appearing here would mean someone
   * had started sending a client-side copy of it — which fails silently when
   * it is one byte out: a 200, a rewritten report, and an empty state that
   * never appears.
   */
  it('sends no document and no body at all', () => {
    const operation = todayReportResetOperation();
    expect(operation.method).toBe('POST');
    expect(operation.path).toBe('/api/today/launchpad/report/reset');
    expect(operation.body).toBeUndefined();
  });

  it('decodes the track and the flipped predicate the server answers with', () => {
    const parsed = todayReportResetOperation().responseSchema.parse({
      track_id: 'lp', report_has_noninitial_content: false,
    });
    expect(parsed).toEqual({ track_id: 'lp', report_has_noninitial_content: false });
  });

  /* The read the page load uses is a GET, and it stays one. Stated here beside
     the reset because the pair is the whole contract: exactly one of these two
     writes, and it is never the one on the render path. */
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

describe('the explicit Today launchpad entry', () => {
  it('materialises the launchpad without a client-authored body', () => {
    const operation = todayLaunchpadEnsureOperation();
    expect(operation.method).toBe('POST');
    expect(operation.path).toBe('/api/today/launchpad/ensure');
    expect(operation.body).toBeUndefined();
    expect(operation.responseSchema.parse({ track_id: 'lp' })).toEqual({ track_id: 'lp' });
  });
});
