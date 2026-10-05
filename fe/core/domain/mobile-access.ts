import type { FailureTable, WriteClass, WriteFailure, WriteText } from './failure-class.js';

/*
 * What a failed write on `/api/mobile/*` means (#2131). Each table is read through `writeFailureText`: a refusal shows
 * the server's reason, `done` shows nothing, and `unknown` shows the fixed sentence; the pane reads the list again
 * after every write. The server answers every owner-side refusal with a status other than 401. A 401 is the
 * transport's session signal: the session is gone, and a table's `unauthorized` entry only names what the pane says.
 */

/** Neither sentence speaks of the connection: that is the global indicator's. */
export const MOBILE_WRITE_TEXT: WriteText = Object.freeze({
  refused: 'This change was not made.',
  unknown: 'Neige could not confirm this change. The list below shows what is in effect.',
});

export const MOBILE_READ_TEXT: WriteText = Object.freeze({
  refused: 'Mobile access status was refused.',
  unknown: 'Mobile access status could not be read.',
});

/**
 * Set-to-state writes: turning access on or off, Tailnet sign-in and sign-out, and cancelling a scan enrollment
 * (a no-op when the slot is already gone). Each refuses with 400 (not configured, or the provider failed) or 403
 * (not a real owner login). A 5xx may follow a partial change, so it is `unknown`.
 */
export const MOBILE_STATE_FAILURES: FailureTable<WriteFailure> = Object.freeze({
  rules: Object.freeze([
    Object.freeze({ status: Object.freeze([400, 403]), is: 'refused' as const }),
  ]),
  unauthorized: 'refused',
  otherwise: 'unknown',
});

/**
 * `POST /api/mobile/pairings` and `POST /api/mobile/enrollments`. Refusals: 400 (limits, the issuer), 403, and
 * 409 `conflict` (access is off, or the scan slot was cancelled while it was being created). A retry after an
 * `unknown` answer cannot leave two live QR codes: the server replaces the previous unclaimed one.
 */
export const MOBILE_INVITATION_FAILURES: FailureTable<WriteFailure> = Object.freeze({
  rules: Object.freeze([
    Object.freeze({ status: Object.freeze([400, 403]), is: 'refused' as const }),
    Object.freeze({ status: Object.freeze([409]), code: 'conflict', is: 'refused' as const }),
  ]),
  unauthorized: 'refused',
  otherwise: 'unknown',
});

/**
 * `POST /api/mobile/pairings/{id}/approve`. A repeated approve, also after the phone joined, answers 204. Refusals:
 * 404 `not_found` (no pending request: expired, never claimed, or unknown), 409 `conflict` (access is off), 403.
 */
export const MOBILE_APPROVE_FAILURES: FailureTable<WriteFailure> = Object.freeze({
  rules: Object.freeze([
    Object.freeze({ status: Object.freeze([404]), code: 'not_found', is: 'refused' as const }),
    Object.freeze({ status: Object.freeze([409]), code: 'conflict', is: 'refused' as const }),
    Object.freeze({ status: Object.freeze([403]), is: 'refused' as const }),
  ]),
  unauthorized: 'refused',
  otherwise: 'unknown',
});

/** `DELETE /api/mobile/devices/{id}`: 404 `not_found` means the device is already gone, which is what revoke asked for. */
export const MOBILE_REVOKE_FAILURES: FailureTable<WriteClass> = Object.freeze({
  rules: Object.freeze([
    Object.freeze({ status: Object.freeze([404]), code: 'not_found', is: 'done' as const }),
    Object.freeze({ status: Object.freeze([403]), is: 'refused' as const }),
  ]),
  unauthorized: 'refused',
  otherwise: 'unknown',
});

/** The status reads (`GET /api/mobile/access`, `GET /api/mobile/enrollments`): 400 carries the server's reason. */
export const MOBILE_READ_FAILURES: FailureTable<WriteFailure> = Object.freeze({
  rules: Object.freeze([
    Object.freeze({ status: Object.freeze([400, 403]), is: 'refused' as const }),
  ]),
  unauthorized: 'refused',
  otherwise: 'unknown',
});
