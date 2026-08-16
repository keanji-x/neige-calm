export type {
  CardComponentProps,
  CardCreateStrategy,
  CardDataMap,
  CardEntry,
  CardKindClaim,
  CardRegistry,
  CardSize,
  KernelCardInput,
  RegisteredCard,
} from './registry.js';
export { createCardRegistry, FALLBACK_SIZE } from './registry.js';
export type {
  CardRecord,
  CardController,
  CardGeometry,
  CardLifecycleSnapshot,
  CardLifecycleStore,
  CardRuntimeCommand,
  CreateCardController,
} from './contracts.js';
export type { CardWheelTargetDecl } from './lifecycle.js';
export type {
  CardControllerCallback,
  CardControllerErrorContext,
  CardHost,
  CardHostOptions,
  CardHostWriter,
  MountedCard,
} from './host.js';
export type { CardHostCapabilities, CardSlotStore } from './contracts.js';
export { createCardHost } from './host.js';
// Built-in composition. `cards-public-entry-only` forbids deep imports into
// this module, so this is the only door onto `builtins/`.
export type { BuiltinCardType } from './builtins/register.js';
export { BUILTIN_CARD_ORDER, registerAvailableBuiltinCards } from './builtins/register.js';
// The spec discriminator itself, not a copy of it. Whether a card is hidden
// from CARDS and whether the conversation drawer exists are the same question,
// so app must ask the entry's own predicate rather than re-implement it.
export { isSpecHarnessPayload } from './builtins/spec.js';
export type {
  UnknownCardSlot,
  VisibleCardSlot,
  WaveCardPartition,
} from './builtins/headless-filter.js';
export { partitionWaveCards } from './builtins/headless-filter.js';
