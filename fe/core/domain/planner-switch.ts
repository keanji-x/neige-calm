import { NotSentError } from './failure-class.js';
import { z } from 'zod';
import type { ApiFailure, ApiOperation } from '../api/types.js';

export type PlannerSwitchRequest = Readonly<{
  target_assistant_card_id: string;
  expected_planner_card_id: string;
  expected_revision: number;
}>;
export const plannerSwitchResponseSchema = z.object({
  track_id: z.string(), planner_card_id: z.string(), demoted_card_id: z.string(),
  planner_revision: z.number().int().nonnegative(),
});
export type PlannerSwitchResponse = z.infer<typeof plannerSwitchResponseSchema>;
export function plannerSwitchOperation(trackId: string, body: PlannerSwitchRequest): ApiOperation<PlannerSwitchResponse> {
  return { method: 'POST', path: `/api/tracks/${encodeURIComponent(trackId)}/planner/switch`, body,
    responseSchema: plannerSwitchResponseSchema };
}

// Protocol closed set; provider capability remains server-owned. English is the current UI default.
export const PLANNER_SWITCH_COPY = Object.freeze({
  planner_user_required: Object.freeze({ zh: '只有用户本人可以切换 Planner。', en: 'Only a user can switch the Planner.' }),
  planner_target_invalid: Object.freeze({ zh: '请选择本 Track 中另一个 Assistant 卡。', en: 'Choose another Assistant card in this Track.' }),
  planner_changed: Object.freeze({ zh: 'Planner 已发生变化，请刷新后重试。', en: 'The Planner changed. Refresh before trying again.' }),
  planner_historical_storage: Object.freeze({ zh: '此对话使用旧版存储，暂不支持切换。', en: 'This conversation uses historical storage and cannot switch yet.' }),
  planner_binding_unsupported: Object.freeze({ zh: '此对话的角色绑定或配置不支持切换。', en: 'This conversation’s role binding or configuration does not support switching.' }),
  planner_pending_messages: Object.freeze({ zh: '仍有待处理消息，处理完后再试。', en: 'Wait for pending messages to finish.' }),
  planner_pending_submission: Object.freeze({ zh: '仍有待处理提交，完成后再试。', en: 'Wait for pending submissions to finish.' }),
  planner_turn_in_flight: Object.freeze({ zh: '对话正在执行，结束后再试。', en: 'Wait for the current turn to finish.' }),
  planner_session_unavailable: Object.freeze({ zh: '无法确认会话可安全切换，请恢复会话后再试。', en: 'The session cannot be confirmed safe to switch. Restore it before trying again.' }),
  planner_pending_observations: Object.freeze({ zh: 'Planner 仍有待处理事件，处理完后再试。', en: 'Wait for the Planner’s pending observations to finish.' }),
  planner_open_ask: Object.freeze({ zh: '仍有待回答的问题，请先处理。', en: 'Resolve the open questions first.' }),
});
export const PLANNER_SWITCH_UNCONFIRMED = Object.freeze({
  zh: '切换结果尚未确认，请刷新后检查。', en: 'Planner switching is unconfirmed. Refresh to check.',
});
export const PLANNER_SWITCH_ARGUMENTS = 'Use /planner without arguments.';

export function plannerSwitchFailure(failure: ApiFailure | NotSentError | null): Readonly<{ message: string; cardId: string | null; unconfirmed: boolean }> {
  if (failure instanceof NotSentError) return { message: 'Planner switching was not sent.', cardId: null, unconfirmed: false };
  if (failure?.kind === 'http' && failure.status < 500) {
    const copy = Object.hasOwn(PLANNER_SWITCH_COPY, failure.code)
      ? PLANNER_SWITCH_COPY[failure.code as keyof typeof PLANNER_SWITCH_COPY] : undefined;
    const body = failure.body;
    const cardId = typeof body === 'object' && body !== null && 'card_id' in body && typeof body.card_id === 'string' ? body.card_id : null;
    return { message: copy?.en ?? failure.message, cardId, unconfirmed: false };
  }
  if (failure?.kind === 'unauthorized') return { message: failure.message, cardId: null, unconfirmed: false };
  return { message: PLANNER_SWITCH_UNCONFIRMED.en, cardId: null, unconfirmed: true };
}
