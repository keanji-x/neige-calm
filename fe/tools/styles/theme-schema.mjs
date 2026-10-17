import { z } from 'zod';

const name = z.string().regex(/^[a-z][a-z0-9-]*$/);
const step = z.strictObject({
  size: z.number().min(10).max(64),
  line: z.number().min(10).max(80),
  tracking: z.number().min(-.1).max(.1),
}).refine(value => value.line >= value.size, 'line height is smaller than font size');
const recipe = z.strictObject({ family: name, step: z.string().min(1), weight: name });

/** Editable recipes live only in JSON. Schema validates shape and references, not a second role registry. */
export const typographySchema = z.strictObject({
  families: z.strictObject({ ui: z.string().min(1), reading: z.string().min(1), code: z.string().min(1) }),
  scale: z.record(z.string().regex(/^[1-9]$/), step).refine(value => Object.keys(value).length === 9, 'Scale requires all nine steps'),
  exceptions: z.strictObject({ editor: step, compact: step }),
  weights: z.strictObject({ regular: z.number().min(100).max(900), medium: z.number().min(100).max(900),
    semibold: z.number().min(100).max(900), bold: z.number().min(100).max(900) }),
  roles: z.record(name, z.union([recipe, name])).refine(value => Object.keys(value).length > 0, 'Roles cannot be empty'),
}).superRefine((value, context) => {
  const steps = { ...value.scale, ...value.exceptions };
  /** @type {Record<string, object>} */
  const registries = { family: value.families, step: steps, weight: value.weights };
  const recipes = new Map();
  for (const [id, binding] of Object.entries(value.roles)) {
    if (typeof binding === 'string') {
      if (!Object.hasOwn(value.roles, binding)) context.addIssue({ code: 'custom', path: ['roles', id],
        message: `Unknown role reference ${binding}` });
      else if (typeof value.roles[binding] === 'string') context.addIssue({ code: 'custom', path: ['roles', id],
        message: 'Alias must reference a concrete recipe' });
      continue;
    }
    const signature = JSON.stringify([binding.family, binding.step, binding.weight]);
    if (recipes.has(signature)) context.addIssue({ code: 'custom', path: ['roles', id],
      message: `Duplicate role recipe; reference ${recipes.get(signature)}` });
    recipes.set(signature, id);
    for (const [field, reference] of Object.entries(binding)) {
      if (!Object.hasOwn(registries[field], reference)) context.addIssue({ code: 'custom',
        path: ['roles', id, field], message: `Unknown ${field} reference ${reference}` });
    }
  }
  // Generator row validation and the vendor body/code bridge depend on these public semantics.
  for (const id of ['ui', 'reading', 'code']) {
    if (!Object.hasOwn(value.roles, id)) context.addIssue({ code: 'custom',
      path: ['roles', id], message: `Missing required semantic role ${id}` });
  }
}).transform(({ roles, ...value }) => ({
  ...value,
  roles: Object.fromEntries(Object.entries(roles).map(([id, binding]) =>
    [id, typeof binding === 'string' ? roles[binding] : binding])),
}));
