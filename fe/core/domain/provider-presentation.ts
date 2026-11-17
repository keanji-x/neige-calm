import type { AgentProvider } from '../api/generated/wire.js';
/** Shared provider display vocabulary; glyphs are existing UI icon names, never brand assets. */
const PROVIDERS = Object.freeze({
  codex: Object.freeze({ label: 'Codex', icon: 'codex' as const }),
  claude: Object.freeze({ label: 'Claude', icon: 'claude' as const }),
  opencode: Object.freeze({ label: 'OpenCode', icon: 'agent' as const }),
  grok: Object.freeze({ label: 'Grok', icon: 'agent' as const }),
  antigravity: Object.freeze({ label: 'Antigravity', icon: 'agent' as const }),
});
export function providerPresentation(provider: AgentProvider | null | undefined) {
  return provider == null ? null : PROVIDERS[provider];
}
/** Historical aliases normalize only at the presentation boundary. The stored mechanism remains intact. */
export function taskProvider(kind: string | null, provider?: AgentProvider | null): AgentProvider | null {
  return provider ?? (kind === 'codex' || kind === 'claude' ? kind : null);
}
