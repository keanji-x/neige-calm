import { z } from 'zod/v4';
import { object, string } from 'zod';
export const Strict = z.object({}).strict();
export const Min = z.string().min(1);
export const Named = object({ title: string() }).strict();
