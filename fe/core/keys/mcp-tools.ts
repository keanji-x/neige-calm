/** The kernel's own MCP tool names, as they appear on the wire. Not exhaustive: an unrecognised tool keeps its wire name. */

/** Everything that changes the report. `commit` is the batched form (blocks + summary + lifecycle). */
export const REPORT_WRITE_TOOLS: readonly string[] = Object.freeze([
  'calm.report.write',
  'calm.report.write_markdown',
  'calm.report.edit',
  'calm.report.blocks.upsert',
  'calm.report.commit',
]);

export const REPORT_MOVE_TOOL = 'calm.report.blocks.move';
export const REPORT_DELETE_TOOL = 'calm.report.blocks.delete';
export const TASK_VERDICT_TOOL = 'calm.task.verdict';
export const PLAN_LIST_TOOL = 'calm.plan.list';

/** Reads of the report. */
export const REPORT_READ_TOOLS: readonly string[] = Object.freeze([
  'calm.report.read',
  'calm.report.blocks.kinds',
  'calm.report.links.backlinks',
]);

/** Every report tool, read or write, is under this prefix: a report tool this file has never
 *  heard of counts as a change, so a new write can never read as silence. */
export const REPORT_TOOL_PREFIX = 'calm.report.';

export const TRACK_TOOL_PREFIX = 'calm.track.';

/** A `calm.track.*` tool that is a WRITE — keep it out of the `TRACK_TOOL_PREFIX` read bucket. */
export const TRACK_RENAME_TOOL = 'calm.track.rename';

/** The planner's way to speak to the reader from a background turn; rendered as an agent turn, text verbatim from `arguments.text`. */
export const USER_NOTIFY_TOOL = 'calm.user.notify';
