import type { CardEntry, CardRegistry, RegisteredCard } from '../registry.js';
import { ASSISTANT_CARD_ENTRY } from './assistant.js';
import { CLAUDE_CARD_ENTRY } from './claude.js';
import { CODEX_CARD_ENTRY } from './codex.js';
import { FILE_VIEWER_CARD_ENTRY } from './file-viewer.js';
import { SPEC_CARD_ENTRY } from './spec.js';
import { TERMINAL_CARD_ENTRY } from './terminal.js';
import { WAVE_REPORT_CARD_ENTRY } from './wave-report.js';

export const BUILTIN_CARD_ORDER = Object.freeze([
  'terminal',
  'codex',
  'spec',
  /* After `spec` and before `claude`: the two headless conversation adapters
     sit together, and both of them depend on `codex` — registered ahead of
     both — refusing their markers explicitly (`codex.ts`). */
  'assistant',
  'claude',
  'wave-report',
  'file-viewer',
  'iframe',
  'plugin-iframe',
] as const);

export type BuiltinCardType = (typeof BUILTIN_CARD_ORDER)[number];

class BuiltinRegistrar {
  readonly #register: (target: CardRegistry) => void;

  private constructor(register: (target: CardRegistry) => void) {
    this.#register = register;
  }

  static of<Card extends RegisteredCard>(
    entry: CardEntry<Card> & { readonly headless: boolean },
  ): BuiltinRegistrar {
    return new BuiltinRegistrar((target) => { target.register(entry); });
  }

  run(target: CardRegistry): void {
    this.#register(target);
  }
}

type BuiltinRegistrarMap = Partial<Record<BuiltinCardType, BuiltinRegistrar>>;

type AssertTrue<Condition extends true> = Condition;

type BuiltinRegistrarSlot = NonNullable<BuiltinRegistrarMap[BuiltinCardType]>;

type SlotRejectsStructuralRegistrars =
  ((target: CardRegistry) => void) extends BuiltinRegistrarSlot
    ? false
    : { run(target: CardRegistry): void } extends BuiltinRegistrarSlot
      ? false
      : true;

export type StructuralRegistrarIsNotAssignable = AssertTrue<SlotRejectsStructuralRegistrars>;

export function registerAvailableBuiltinCards(registry: CardRegistry): void {
  const registrars: BuiltinRegistrarMap = {
    terminal: BuiltinRegistrar.of(TERMINAL_CARD_ENTRY),
    codex: BuiltinRegistrar.of(CODEX_CARD_ENTRY),
    spec: BuiltinRegistrar.of(SPEC_CARD_ENTRY),
    assistant: BuiltinRegistrar.of(ASSISTANT_CARD_ENTRY),
    claude: BuiltinRegistrar.of(CLAUDE_CARD_ENTRY),
    'wave-report': BuiltinRegistrar.of(WAVE_REPORT_CARD_ENTRY),
    'file-viewer': BuiltinRegistrar.of(FILE_VIEWER_CARD_ENTRY),
  };
  for (const type of BUILTIN_CARD_ORDER) {
    registrars[type]?.run(registry);
  }
}
