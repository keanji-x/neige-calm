import { z } from 'zod';
import type { CardWire } from './track.js';

const terminalPresentationSchema = z.object({
  source: z.enum(['manual', 'agent', 'task', 'system', 'plugin', 'unrecorded']),
  in_workbench: z.boolean(),
});
const terminalPayloadSchema = z.object({ terminal_presentation: terminalPresentationSchema });
export type TerminalPresentation = z.infer<typeof terminalPresentationSchema>;

/** Live Terminal cards always carry owner-persisted metadata; historical event payloads stay opaque. */
export function terminalPresentation(card: CardWire): TerminalPresentation {
  if (card.kind !== 'terminal') throw new Error('Terminal presentation requires a terminal card');
  return terminalPayloadSchema.parse(card.payload).terminal_presentation;
}
