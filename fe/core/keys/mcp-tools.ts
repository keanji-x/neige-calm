/**
 * The kernel's own MCP tool names, as they appear on the wire.
 *
 * They live here for the same reason storage keys do: `calm.`-prefixed strings
 * are an API someone else owns, and a rename has to have one place to land. The
 * conversation transcript reads these off `mcpToolCall` rows to say what the
 * agent did in English ("Wrote report") instead of printing a tool name at the
 * reader — the one case where being wrong about a spelling silently downgrades
 * a line rather than breaking anything, which is exactly the kind of wrong that
 * survives for months.
 *
 * The list is not exhaustive and does not need to be: an unrecognised tool
 * keeps its wire name on the line, which stays true no matter what the kernel
 * adds next.
 */

/** Everything that changes the report. `blocks.upsert` is what the spec agent
 *  actually calls today; `write`/`write_markdown`/`edit` are the older and
 *  whole-document routes, all still served. */
export const REPORT_WRITE_TOOLS: readonly string[] = Object.freeze([
  'calm.report.write',
  'calm.report.write_markdown',
  'calm.report.edit',
  'calm.report.blocks.upsert',
]);

export const REPORT_MOVE_TOOL = 'calm.report.blocks.move';
export const REPORT_DELETE_TOOL = 'calm.report.blocks.delete';
export const TASK_VERDICT_TOOL = 'calm.task.verdict';
export const PLAN_LIST_TOOL = 'calm.plan.list';

/** Reads — of the report, and of the wave's tree and history. Every one of
 *  these is a look, never a change. */
export const REPORT_READ_TOOLS: readonly string[] = Object.freeze([
  'calm.report.read',
  'calm.report.blocks.kinds',
  'calm.report.links.backlinks',
]);

export const WAVE_TOOL_PREFIX = 'calm.wave.';
