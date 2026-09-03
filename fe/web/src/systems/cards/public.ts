export type {
  CardAddMenuEntry,
  CardAddPanel,
  CardComponentProps,
  CardCreateField,
  CardCreateStrategy,
  CardDataMap,
  CardEntry,
  CardKindClaim,
  CardRegistry,
  CardSize,
  KernelCardInput,
  RegisteredCard,
} from './registry.js';
export { cardAddMenuEntries, createCardRegistry, FALLBACK_SIZE } from './registry.js';
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
// The track-assistant discriminator, on the same terms (#1189): app decides
// whether a card opens the conversation drawer, and it must decide it with the
// entry's own predicate rather than a second copy of the payload check.
export { isAssistantHarnessPayload } from './builtins/assistant.js';
export type {
  UnknownCardSlot,
  VisibleCardSlot,
  TrackCardPartition,
} from './builtins/headless-filter.js';
export { partitionTrackCards } from './builtins/headless-filter.js';
export { BoardHost } from './ui/board-host.js';
export type { BoardHostItem } from './ui/board-host.js';
export { GRID_COLS, GRID_MARGIN, GRID_ROW_HEIGHT, layoutToPositions, packCards, reconcileLayout } from './ui/layout.js';
export type { GridPlacement, StoredPosition, StoredPositions } from './ui/layout.js';

