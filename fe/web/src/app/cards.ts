import type { CardEntry, CardRegistry } from '../systems/cards/public.js';

export function registerBuiltinCards(
  registry: CardRegistry,
  terminal: CardEntry,
  codex: CardEntry,
  spec: CardEntry,
  claude: CardEntry,
  waveReport: CardEntry,
  fileViewer: CardEntry,
  iframe: CardEntry,
  pluginIframe: CardEntry,
): void {
  for (const entry of [terminal, codex, spec, claude, waveReport, fileViewer, iframe, pluginIframe]) {
    registry.register(entry);
  }
}
