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
/** The Planner's two decisions on a task attempt. */
export const TASK_ACCEPT_TOOL = 'neige_task_accept';
export const TASK_REJECT_TOOL = 'neige_task_reject';
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
export const DEV_PUBLISH_TOOL = 'plugin_gitforge_publish';

/** The planner's one way to ask the reader (#2209); rendered as an agent turn whose text is its questions. */
export const USER_ASK_TOOL = 'neige_user_ask';

/** Mail to another Track of the Area (#2130); its line names the mail by `arguments.summary`. */
export const MAIL_SEND_TOOL = 'neige_mail_send';
