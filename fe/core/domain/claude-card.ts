import { z } from 'zod';

import type { ClaudeInstalledVersion } from '../api/generated/wire.js';
import type { WorkerSessionState } from '../api/schemas.js';
import type { ApiAbortSignal, ApiOperation } from '../api/types.js';
import type { FailureTable, WriteFailure, WriteText } from './failure-class.js';
import { cardWireSchema, type CardWire } from './track.js';

/**
 * #2516: an owner-created Claude card's Update and Resume. Both restart the card through one route; the kernel stops a
 * running child first. The installed version is the server's cached `claude --version`.
 */

export const claudeInstalledVersionSchema: z.ZodType<ClaudeInstalledVersion> = z.object({
  version: z.string().nullable(),
});

/** `GET /api/claude/installed-version`: the server's cached answer; it never runs the CLI. */
export function claudeInstalledVersionOperation(signal?: ApiAbortSignal): ApiOperation<ClaudeInstalledVersion> {
  return {
    method: 'GET', path: '/api/claude/installed-version', responseSchema: claudeInstalledVersionSchema,
    ...(signal === undefined ? {} : { signal }),
  };
}

/** `POST /api/claude/installed-version/refresh`: reads the CLI again, at most once a minute; answers once that is done. */
export function refreshClaudeInstalledVersionOperation(): ApiOperation<undefined> {
  return { method: 'POST', path: '/api/claude/installed-version/refresh', responseSchema: z.undefined() };
}

/** `POST /api/cards/{id}/claude/restart`: the card as it is after the restart, its new runtime included. */
export function restartClaudeCardOperation(cardId: string): ApiOperation<CardWire> {
  return {
    method: 'POST', path: `/api/cards/${encodeURIComponent(cardId)}/claude/restart`, responseSchema: cardWireSchema,
  };
}

/**
 * What a failed restart says. `refused`: the documented answers given before anything is stopped or started (403 not a
 * resumable Claude card, 404 no such card, 409 a child the kernel would not stop or an exit not recorded in time). A
 * 5xx or a lost answer may follow a started child, so it is `unknown`: the card's next state says which.
 */
export const CLAUDE_RESTART_FAILURES: FailureTable<WriteFailure> = Object.freeze({
  rules: Object.freeze([Object.freeze({ status: Object.freeze([403, 404, 409]), is: 'refused' as const })]),
  unauthorized: 'refused',
  otherwise: 'unknown',
});

export const CLAUDE_RESTART_TEXT = Object.freeze({
  update: Object.freeze({ refused: 'Claude was not updated.', unknown: 'The update is unconfirmed.' }),
  resume: Object.freeze({ refused: 'The session was not resumed.', unknown: 'Resuming the session is unconfirmed.' }),
}) satisfies Readonly<Record<ClaudeCardAction, WriteText>>;

export type ClaudeCardAction = 'update' | 'resume';

export type ClaudeCardActionInput = Readonly<{
  /** Created by the owner through the claude-cards route; a task worker's card is the Planner's to recover. */
  ownerCreated: boolean;
  sessionState: WorkerSessionState | null;
  /** The version the card's child started on, when the kernel recorded one. */
  runningVersion: string | null;
  /** The server's installed version, when known. */
  installedVersion: string | null;
}>;

const ENDED_STATES: readonly WorkerSessionState[] = Object.freeze(['exited', 'failed', 'superseded'] as const);

/**
 * The one action a Claude card's head offers, or `null`: only an owner-created card has one. Resume when its session
 * has ended; Update when it runs on a version other than the installed one, both known.
 */
export function claudeCardAction(input: ClaudeCardActionInput): ClaudeCardAction | null {
  if (!input.ownerCreated || input.sessionState === null) return null;
  if (ENDED_STATES.includes(input.sessionState)) return 'resume';
  if (input.sessionState === 'running' && input.runningVersion !== null && input.installedVersion !== null
    && input.runningVersion !== input.installedVersion) return 'update';
  return null;
}

/**
 * What a Claude card is handed to read the installed version and restart itself; built by the app from its transport.
 * `restart` answers with the card as the kernel projects it after the restart.
 */
export type ClaudeCardPort = Readonly<{
  installedVersion(signal: ApiAbortSignal): Promise<ClaudeInstalledVersion>;
  refreshInstalledVersion(): Promise<void>;
  restart(cardId: string): Promise<CardWire>;
}>;
