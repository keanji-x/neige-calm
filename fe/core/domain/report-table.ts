import { z } from 'zod';

export function max2048CodePoints(schema: z.ZodString) {
  return schema.refine((value) => [...value].length <= 2048, { message: 'String must contain at most 2048 character(s)' }).meta({ maxLength: 2048 });
}

export const inlineTableBlockPayloadSchema = z.strictObject({
  columns: z.array(z.strictObject({
    key: max2048CodePoints(z.string().min(1)),
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
  }, { message: 'row keys must be declared column keys' });
