/** The kernel's own MCP tool names, as they appear on the wire. Not exhaustive: an unrecognised tool keeps its wire name. */

/** Everything that changes the report. `commit` is the batched form (blocks + summary); `upsert`
 *  names the retired single-block writer in stored history (#2003 migration). */
export const REPORT_WRITE_TOOLS: readonly string[] = Object.freeze([
  'neige.report.write',
  'neige.report.upsert',
  'neige.report.commit',
]);

/** The retired single-op block delete, as stored history names it (#2003 migration). */
export const REPORT_DELETE_TOOL = 'neige.report.delete';
export const TASK_VERDICT_TOOL = 'neige.task.verdict';
export const PLAN_LIST_TOOL = 'neige.plan.list';

/** Reads of the report. */
export const REPORT_READ_TOOLS: readonly string[] = Object.freeze([
  'neige.report.read',
  'neige.report.kinds',
  'neige.report.backlinks',
]);

/** Every report tool, read or write, is under this prefix: a report tool this file has never
 *  heard of counts as a change, so a new write can never read as silence. */
export const REPORT_TOOL_PREFIX = 'neige.report.';

export const TRACK_TOOL_PREFIX = 'neige.track.';

/** A `neige.track.*` tool that is a WRITE — keep it out of the `TRACK_TOOL_PREFIX` read bucket. */
export const TRACK_RENAME_TOOL = 'neige.track.rename';

/** Development publication: pushes the candidate branch and opens or reuses its PR. */
export const DEV_PUBLISH_TOOL = 'neige.dev.publish';

/** The planner's way to speak to the reader from a background turn; rendered as an agent turn, text verbatim from `arguments.text`. */
export const USER_NOTIFY_TOOL = 'neige.user.notify';
