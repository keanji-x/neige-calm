/** The kernel's own MCP tool names, as they appear on the wire. Not exhaustive: an unrecognised tool keeps its wire name. */

/** Everything that changes the report. `commit` is the batched form (blocks + summary); `upsert`
 *  names the retired single-block writer in stored history (#2003 migration). */
export const REPORT_WRITE_TOOLS: readonly string[] = Object.freeze([
  'neige_report_write',
  'neige_report_upsert',
  'neige_report_commit',
]);

/** The retired single-op block delete, as stored history names it (#2003 migration). */
export const REPORT_DELETE_TOOL = 'neige_report_delete';
export const TASK_VERDICT_TOOL = 'neige_task_verdict';
export const TASK_LS_TOOL = 'neige_task_ls';

/** Reads of the report. */
export const REPORT_READ_TOOLS: readonly string[] = Object.freeze([
  'neige_report_read',
  'neige_report_describe',
  'neige_link_ls',
]);

/** Every report tool, read or write, is under this prefix: a report tool this file has never
 *  heard of counts as a change, so a new write can never read as silence. */
export const REPORT_TOOL_PREFIX = 'neige_report_';

export const TRACK_TOOL_PREFIX = 'neige_track_';

/** A `neige_track_*` tool that is a WRITE — keep it out of the `TRACK_TOOL_PREFIX` read bucket. */
export const TRACK_RENAME_TOOL = 'neige_track_rename';

/** Development publication: pushes the candidate branch and opens or reuses its PR. */
export const DEV_PUBLISH_TOOL = 'neige_dev_publish';

/** The planner's way to speak to the reader from a background turn; rendered as an agent turn, text verbatim from `arguments.text`. */
export const USER_NOTIFY_TOOL = 'neige_user_notify';
