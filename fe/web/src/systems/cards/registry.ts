import type { ComponentType } from 'react';
import type { CardHostCapabilities } from './contracts.js';

export interface CardDataMap {
  readonly _cardDataMapBrand?: never;
}

export type RegisteredCard = CardDataMap[Exclude<keyof CardDataMap, '_cardDataMapBrand'>];

export interface KernelCardInput {
  readonly id: string;
  readonly kind: string;
  readonly payload: unknown;
}

export interface CardSize {
  readonly w: number;
  readonly h: number;
  readonly minW: number;
  readonly minH: number;
}

export const FALLBACK_SIZE: CardSize = Object.freeze({ w: 4, h: 6, minW: 3, minH: 3 });

export type CardKindClaim =
  | Readonly<{ mode: 'exact'; kind: string }>
  | Readonly<{ mode: 'prefix'; prefix: string }>;

export type CardCreateStrategy =
  | Readonly<{ mode: 'generic'; buildPayload(input: Readonly<Record<string, string>>): unknown }>
  | Readonly<{ mode: 'atomic'; submit(input: Readonly<Record<string, string>>): Promise<{ cardId: string }> }>
  | Readonly<{ mode: 'catalog'; catalog: string }>
  | Readonly<{ mode: 'kernel-minted-only' }>;

export interface CardComponentProps<Card extends { readonly id: string; readonly type: string } = RegisteredCard> {
  readonly card: Card;
  readonly host: CardHostCapabilities;
  /**
   * The board's delete, when this card may be deleted and the board was given
   * one. A card draws its own head, so the head's × has to arrive as a prop —
   * and it arrives already resolved: `undefined` here means "no delete on this
   * card", whether because the kernel owns the row or because the surface
   * hosting it offers no delete at all. An entry must not decide that for
   * itself, which is why there is no `deletable` alongside it to re-derive.
   */
  readonly onRemove?: () => void;
}

/**
 * One input the create form collects before a card of this kind can be minted.
 *
 * `kind` is what the form renders, not what the value *is*: every value reaches
 * the create path as a string, because that is what both doors the kernel
 * offers take (a JSON payload field, or a typed create body's string member).
 * `directory` and `file` differ only in what the picker will let you land on.
 */
export interface CardCreateField {
  readonly key: string;
  readonly label: string;
  readonly kind: 'text' | 'directory' | 'file';
  readonly required?: boolean;
  readonly placeholder?: string;
  /** Rendered under the field. One sentence — this is a create form, not docs. */
  readonly hint?: string;
}

/**
 * The entry's presence in the track panel's add menu.
 *
 * Declaring it here rather than in a list held by the panel is the whole point:
 * the menu is then a *projection of the registry*, so a card kind this build
 * cannot draw can never be offered, and adding a kind is one edit rather than
 * two that can drift.
 *
 * No `fields` means no form: picking it creates the card immediately, which is
 * the honest shape for a kind that has nothing to ask (terminal).
 */
export interface CardAddPanel {
  readonly label: string;
  readonly fields?: readonly CardCreateField[];
}

export interface CardEntry<Card extends { readonly id: string; readonly type: string } = RegisteredCard> {
  readonly type: Card['type'];
  readonly component: ComponentType<CardComponentProps<Card>>;
  readonly defaultSize: CardSize;
  readonly claim?: CardKindClaim;
  readonly title?: (card: Card) => string;
  readonly accessibleName?: (card: Card) => string;
  readonly create?: CardCreateStrategy;
  readonly addPanel?: CardAddPanel;
  readonly fromKernel?: (card: KernelCardInput) => Card | null;
}

/** One row of the add menu: what to show, and what to create if it is picked. */
export interface CardAddMenuEntry {
  readonly type: string;
  readonly label: string;
  readonly fields: readonly CardCreateField[];
}

/**
 * The add menu, read off the registry in registration order (which is
 * `BUILTIN_CARD_ORDER`) — so the menu's order is the built-in order, declared
 * once, rather than a second list to keep in step.
 *
 * `catalog` and `kernel-minted-only` entries are excluded whatever they declare:
 * the first is created from somewhere else entirely, and the second is a kind
 * only the kernel may mint (the planner harness, the track report). Offering either
 * would be the menu promising a create that has no endpoint behind it.
 */
export function cardAddMenuEntries(registry: CardRegistry): readonly CardAddMenuEntry[] {
  const rows: CardAddMenuEntry[] = [];
  for (const entry of registry.entries()) {
    const addPanel = entry.addPanel;
    if (addPanel === undefined) continue;
    if (entry.create === undefined) continue;
    if (entry.create.mode === 'catalog' || entry.create.mode === 'kernel-minted-only') continue;
    rows.push(Object.freeze({
      type: entry.type,
      label: addPanel.label,
      fields: Object.freeze([...(addPanel.fields ?? [])]),
    }));
  }
  return Object.freeze(rows);
}

export interface CardRegistry {
  register<Card extends RegisteredCard>(entry: CardEntry<Card>): void;
  get(type: string): CardEntry | undefined;
  resolve(card: KernelCardInput): RegisteredCard | null;
  entries(): readonly CardEntry[];
}

function validateEntry(entry: CardEntry, exact: Map<string, CardEntry>, prefix: Map<string, CardEntry>): void {
  if (entry.title === undefined) throw new Error(`EntryMissingMetadata(${entry.type}, title)`);
  if (entry.accessibleName === undefined) throw new Error(`EntryMissingMetadata(${entry.type}, accessibleName)`);
  if (entry.create === undefined) throw new Error(`MissingCreateStrategy(${entry.type})`);
  if (entry.create.mode === 'generic' && entry.claim?.mode !== 'exact') {
    throw new Error(`GenericCreateRequiresExactClaim(${entry.type})`);
  }
  if (entry.refreshBacking === 'controller' && entry.createController === undefined) {
    throw new Error(`RefreshBackingMissingController(${entry.type})`);
  }
  if (entry.claim?.mode === 'exact') {
    const previous = exact.get(entry.claim.kind);
    if (previous !== undefined && previous.type !== entry.type) {
      throw new Error(`DuplicateExactClaim(${entry.claim.kind})`);
    }
  }
  if (entry.claim?.mode === 'prefix') {
    const previous = prefix.get(entry.claim.prefix);
    if (previous !== undefined && previous.type !== entry.type) {
      throw new Error(`DuplicatePrefixClaim(${entry.claim.prefix})`);
    }
  }
}

export function createCardRegistry(): CardRegistry {
  const entries = new Map<string, CardEntry>();
  const exact = new Map<string, CardEntry>();
  const prefix = new Map<string, CardEntry>();
  return Object.freeze({
    register<Card extends RegisteredCard>(typedEntry: CardEntry<Card>): void {
      const entry = typedEntry as unknown as CardEntry;
      validateEntry(entry, exact, prefix);
      const previous = entries.get(entry.type);
      if (previous?.claim?.mode === 'exact' && exact.get(previous.claim.kind) === previous) {
        exact.delete(previous.claim.kind);
      }
      if (previous?.claim?.mode === 'prefix' && prefix.get(previous.claim.prefix) === previous) {
        prefix.delete(previous.claim.prefix);
      }
      entries.set(entry.type, entry);
      if (entry.claim?.mode === 'exact') exact.set(entry.claim.kind, entry);
      if (entry.claim?.mode === 'prefix') prefix.set(entry.claim.prefix, entry);
    },
    get: (type: string) => entries.get(type),
    resolve(card: KernelCardInput): RegisteredCard | null {
      const tried = new Set<CardEntry>();
      const exactEntry = exact.get(card.kind);
      if (exactEntry !== undefined) tried.add(exactEntry);
      const exactResult = exactEntry?.fromKernel?.(card);
      if (exactResult != null) return exactResult;

      let longest: CardEntry | undefined;
      let longestLength = -1;
      for (const [candidate, entry] of prefix) {
        if (card.kind.startsWith(candidate) && candidate.length > longestLength) {
          longest = entry;
          longestLength = candidate.length;
        }
      }
      if (longest !== undefined) tried.add(longest);
      const prefixResult = longest?.fromKernel?.(card);
      if (prefixResult != null) return prefixResult;

      for (const entry of entries.values()) {
        if (tried.has(entry) || entry.fromKernel === undefined) continue;
        const result = entry.fromKernel(card);
        if (result != null) return result;
      }
      return null;
    },
    entries: () => Object.freeze([...entries.values()]),
  });
}
