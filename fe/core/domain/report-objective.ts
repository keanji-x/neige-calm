import { z } from 'zod';
import { max2048CodePoints } from './report-table.js';

function text() {
  return max2048CodePoints(z.string()).refine(value => value.trim() !== '', 'Must not be blank');
}
function u32() { return z.number().int().min(1).max(4294967295); }
function u64() { return z.number().min(1).max(Number(0xffffffffffffffffn)).refine(Number.isInteger); }

/** Mirrors report_blocks/objective.schema.json and ObjectiveDefinition; runtime usage is not block data. */
export const objectiveBlockPayloadSchema = z.strictObject({
  key: z.string().regex(/^[a-z0-9][a-z0-9._-]{0,63}$/),
  title: text(),
  mode: z.union([z.literal('converge'), z.strictObject({ loop: z.strictObject({ plateau: u32() }) })]),
  checks: z.array(z.strictObject({
    id: text(),
    role: z.enum(['invariant', 'ratchet', 'goal']),
    config: z.discriminatedUnion('kind', [
      z.strictObject({ kind: z.literal('command'), params: z.strictObject({ command: text() }) }),
      z.strictObject({ kind: z.literal('metric'), params: z.strictObject({
        command: text(), direction: z.enum(['lower', 'higher']), tolerance: z.number().nonnegative().optional(),
      }) }),
      z.strictObject({ kind: z.literal('evaluator'), params: z.strictObject({
        rubric: z.array(z.strictObject({ id: text(), criterion: text() })).min(1)
          .refine(items => new Set(items.map(item => item.id)).size === items.length, 'Rubric ids must be unique'),
        samples: u32(), aggregate: z.enum(['majority', 'median']),
      }) }),
      z.strictObject({ kind: z.literal('fact'), params: z.strictObject({
        fact: z.enum(['task_accepted', 'pr_merged', 'ask_answered']), subject: text(),
      }) }),
    ]),
  })).min(1),
  constraints: z.array(text()),
  budget: z.strictObject({ turns: u32(), tokens: u64(), wall_secs: u64() }),
  compact: z.enum(['every_settle', 'on_close', 'none']),
}).refine(value => new Set(value.checks.map(check => check.id)).size === value.checks.length,
  'Check ids must be unique')
  .refine(value => value.checks.some(check => check.role === (value.mode === 'converge' ? 'goal' : 'ratchet')),
    'Mode requires a matching check role');

export type ObjectiveBlockPayload = z.infer<typeof objectiveBlockPayloadSchema>;
