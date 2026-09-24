import { z } from 'zod';

export function max2048CodePoints(schema: z.ZodString) {
  return schema.refine((value) => [...value].length <= 2048, { message: 'String must contain at most 2048 character(s)' }).meta({ maxLength: 2048 });
}

/** Inspect input before Zod can drop __proto__; inherited properties are not table cells. */
export function rejectReservedObjectKeys(value: unknown, ctx: z.RefinementCtx) {
  const pending = [value];
  const visited = new Set<object>();
  while (pending.length > 0) {
    const current = pending.pop();
    if (current === null || typeof current !== 'object' || visited.has(current)) continue;
    visited.add(current);
    for (const [key, child] of Object.entries(current)) {
      if (Object.hasOwn(Object.prototype, key)) {
        ctx.addIssue({ code: 'custom', message: 'Reserved object key' });
        return;
      }
      pending.push(child);
    }
  }
}

export const inlineTableBlockPayloadSchema = z.unknown().superRefine(rejectReservedObjectKeys).pipe(z.strictObject({
  columns: z.array(z.strictObject({
    key: max2048CodePoints(z.string().min(1).regex(/^(?!(?:__proto__|constructor|__defineGetter__|__defineSetter__|hasOwnProperty|__lookupGetter__|__lookupSetter__|isPrototypeOf|propertyIsEnumerable|toString|valueOf|toLocaleString)$)/,
      'Reserved table column key')),
    label: max2048CodePoints(z.string()),
    align: z.enum(['left', 'right']).nullish(),
  })).min(1).max(32),
  rows: z.array(z.record(
    z.string(),
    z.union([max2048CodePoints(z.string()), z.number(), z.null()]),
  )).max(500),
  caption: max2048CodePoints(z.string()).nullish(),
  highlight: max2048CodePoints(z.string()).nullish(),
})
  .refine((table) => new Set(table.columns.map((column) => column.key)).size === table.columns.length,
    { message: 'column keys must be unique' })
  .refine((table) => {
    const keys = new Set(table.columns.map((column) => column.key));
    return table.rows.every((row) => Object.keys(row).every((key) => keys.has(key)));
  }, { message: 'row keys must be declared column keys' }));
