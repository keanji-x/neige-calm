import { z } from 'zod';
declare const x: unknown;
export const parsed = z.object({}).parse(x);
