import { z } from 'zod';

export type GuideStep = Readonly<{
  id: string; target: string | readonly string[]; title: string; description: string;
  advance: 'next' | 'activate';
  locate?: string;
  via?: readonly string[];
  outcome?: string;
  example?: Readonly<{ label: string; summary: string; text: string }>;
}>;
export type GuideTour = Readonly<{ id: string; title: string; priority: number; exclusive: boolean; requires: readonly string[]; steps: readonly GuideStep[] }>;
export type GuideCatalog = Readonly<{ version: number; tours: readonly GuideTour[] }>;

/** Repository-authored data only: no HTML, scripts, CSS selectors or product actions. */
export function parseGuideCatalog(input: unknown): GuideCatalog {
  const identifier = z.string().regex(/^[a-z][a-z0-9-]*$/);
  return z.object({
    version: z.number().int().positive(),
    tours: z.array(z.object({
      id: identifier, title: z.string().min(1), priority: z.number().int(), exclusive: z.boolean(), requires: z.array(identifier),
      steps: z.array(z.object({
        id: identifier, target: z.union([identifier, z.array(identifier).min(1)]), title: z.string().min(1),
        description: z.string().min(1), advance: z.enum(['next', 'activate']),
        locate: z.string().min(1).optional(), via: z.array(identifier).optional(),
        outcome: z.string().min(1).optional(),
        example: z.object({ label: z.string().min(1), summary: z.string().min(1), text: z.string().min(1) }).strict().optional(),
      }).strict()).min(1),
    }).strict()).min(1),
  }).strict().superRefine((catalog, context) => {
    const tourIds = new Set<string>();
    const stepIds = new Set<string>();
    for (const tour of catalog.tours) {
      if (tourIds.has(tour.id)) context.addIssue({ code: 'custom', message: `Duplicate tour: ${tour.id}` });
      tourIds.add(tour.id);
      for (const step of tour.steps) {
        if (stepIds.has(step.id)) context.addIssue({ code: 'custom', message: `Duplicate step: ${step.id}` });
        stepIds.add(step.id);
      }
    }
  }).parse(input);
}
