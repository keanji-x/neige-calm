import { z } from 'zod';
export const CardSchema = z.object({ title: z.string() });
