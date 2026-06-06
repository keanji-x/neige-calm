// Card-type registry.
//
// The kernel dispatches every Card through `kind: string`; the UI maps that
// string (via `api/adapt.ts`) to a discriminated `WaveCardData` variant and
// renders the right component. Before M3 those three step lookups were three
// 5-case switches scattered across `ui.tsx`, `WaveGrid.tsx`, and
// `api/adapt.ts`. This module collapses them into one `Map<type, CardEntry>`
// so plugin entries (Slice F) can `.set()` themselves into the same dispatch
// table at runtime without the dispatcher caring.
//
// Built-ins register at app boot via `registerBuiltins()` from
// `cards/builtins/index.ts`; plugin cards will register lazily as their
// iframes mount.
//
// Plugin card kinds: post-M4 the registry only accepts the canonical
// `ui://<plugin>/<view>` resource URI. The legacy `plugin:<id>:<view>` form
// is rejected — `PluginIframeEntry.fromKernel` returns null on it, so the
// adapter falls through and `renderCard` logs a one-shot warning.

import {
  createContext,
  createElement,
  useEffect,
  useContext,
  useMemo,
  useRef,
  type Dispatch,
  type FC,
  type ReactNode,
  type SetStateAction,
} from 'react';
import type { WaveCardData } from '../types';
import type { KernelCard } from '../api/wire';
import { useState } from '../shared/state';
import {
  createCardLifecycleStore,
  sameGeometry,
  type CardController,
  type CardLifecycleSnapshot,
  type CardLifecycleStore,
  type CardRuntimeCommand,
} from './lifecycle';

export interface CardSize {
  w: number;
  h: number;
  minW: number;
  minH: number;
}

/**
 * Minimal JSON-Schema subset the bundled `SchemaForm` renders. Intentionally
 * not the full JSON-Schema spec — we only need enough to drive the create
 * dialog for built-in card kinds. Plugins requiring richer schemas should
 * carry their own renderer through the plugin host.
 */
export interface CreateField {
  /** Field key on the resulting body object. */
  key: string;
  /** Label rendered above the input. */
  label: string;
  /**
   * Storage type — controls the rendered widget.
   *
   * `'directory'` / `'file'` swap the plain text input for
   * `DirectoryPicker`: a read-only field with a browser that walks the
   * host filesystem via `GET /api/fs/listdir`. Picked for codex's `cwd`
   * and the file-viewer card path so users don't have to remember
   * absolute paths.
   */
  type: 'string' | 'textarea' | 'enum' | 'directory' | 'file';
  /** Required for `type: 'enum'`. */
  options?: string[];
  /** Default value pre-filled on first render. */
  default?: string;
  /** Optional placeholder shown when the field is empty. */
  placeholder?: string;
  /** True forces the input non-empty before the form will submit. */
  required?: boolean;
}

export interface CreateSchema<TInput = Record<string, string>> {
  /** Ordered field list — rendered top-to-bottom. */
  fields: CreateField[];
  parse?(values: Record<string, string>): TInput;
}

/** Common props every built-in card component receives. Cards must forward
 *  `onClose` (when provided) to `<CardHead>` so the X button renders inside
 *  the head. Optional: contexts that own the close affordance elsewhere
 *  (e.g. WaveList's row-level button) simply pass `undefined`. */
export interface CardComponentProps<T extends WaveCardData = WaveCardData> {
  card: T;
  onClose?: () => void;
  deletable?: boolean;
}

export type CardKindClaim =
  | { mode: 'exact'; kind: string }
  | { mode: 'prefix'; prefix: string };

export interface CardCreateContext {
  themeRgb: { fg: string; bg: string };
}

export interface CardCreateResult {
  cardId: string;
  raw?: unknown;
}

export type CardCreateStrategy<TInput> =
  | { mode: 'generic'; buildPayload(input: TInput): unknown }
  | {
      mode: 'atomic';
      submit(
        waveId: string,
        input: TInput,
        ctx: CardCreateContext,
      ): Promise<CardCreateResult>;
    }
  | { mode: 'catalog'; catalog: string }
  | { mode: 'kernel-minted-only' };

export type CardIconName = 'refresh' | 'edit' | 'reset';

export interface CardInstanceCtx {
  cardId: string;
  deletable: boolean;
  /**
   * Hook-like keyed state accessor for entry.actions().
   *
   * This is a normal function, not a React hook. CardHead calls
   * entry.actions(card, ctx) during render, and each action can read/write a
   * provider-owned state slot by key. The first read initializes the slot;
   * later reads of the same cardId + key return the same value and ignore
   * their initial argument. Setters resolve functional updates against the
   * current slot value and force a provider re-render, which makes this act
   * like a namespaced useState shared by the card head and card body.
   */
  useInstance<S>(key: string, initial: S): [S, Dispatch<SetStateAction<S>>];
}

export type CardAction =
  | {
      kind: 'button';
      id: string;
      label: string;
      icon: CardIconName;
      placement: 'head';
      run(): void;
      disabled?: boolean;
    }
  | {
      kind: 'imperative';
      id: string;
      placement: 'head';
      render(ctx: CardInstanceCtx): ReactNode;
    };

export interface CardEntry<
  T extends WaveCardData = WaveCardData,
  TInput = Record<string, string>,
> {
  /** The discriminator value used in `T['type']`, e.g. `'terminal'`, `'doc'`,
   *  or the sentinel `'plugin'` for `ui://`-backed iframe cards. */
  type: T['type'] | string;
  Component: FC<CardComponentProps<T>>;
  defaultSize: CardSize;
  /** Optional — kernel→UI adaptation. Receives the raw KernelCard;
   *  return null if this entry doesn't claim that kernel card. */
  fromKernel?: (k: KernelCard) => T | null;
  /** Optional — when present, the entry appears in the AddPanel menu.
   *  Slice G iterates this. */
  addPanel?: {
    label: string;
    /** When present, picking this entry from the AddPanel menu shows an
     *  inline config card rendered by `SchemaForm` instead of immediately
     *  creating the card. Omit for zero-config kinds (current terminal). */
    createSchema?: CreateSchema<TInput>;
  };
  claim?: CardKindClaim;
  title(card: T): string;
  accessibleName(card: T): string;
  create?: CardCreateStrategy<TInput>;
  actions?(card: T, ctx: CardInstanceCtx): CardAction[];
}

const REGISTRY = new Map<string, CardEntry<WaveCardData>>();
const EXACT_CLAIMS = new Map<string, CardEntry<WaveCardData>>();
const PREFIX_CLAIMS = new Map<string, CardEntry<WaveCardData>>();

export const LEGACY_CREATE_KINDS: ReadonlySet<string> = new Set([
  'terminal',
  'codex',
  'claude',
]);

/** Fallback size for unknown card types. Sane mid-range default that fits
 *  any of the built-in shapes; we'd rather render a slightly-wrong-sized
 *  placeholder than throw. */
const FALLBACK_SIZE: CardSize = { w: 4, h: 6, minW: 3, minH: 3 };

const warned = new Set<string>();
function warnOnce(key: string, msg: string) {
  if (warned.has(key)) return;
  warned.add(key);
  // eslint-disable-next-line no-console
  console.warn(msg);
}

export function registerCard<T extends WaveCardData>(entry: CardEntry<T>): void {
  if (!entry.title) throw new Error(`EntryMissingMetadata(${entry.type}, title)`);
  if (!entry.accessibleName) {
    throw new Error(`EntryMissingMetadata(${entry.type}, accessibleName)`);
  }
  if (entry.create?.mode === 'generic' && entry.claim?.mode !== 'exact') {
    throw new Error(`GenericCreateRequiresExactClaim(${entry.type})`);
  }
  if (entry.create === undefined && !LEGACY_CREATE_KINDS.has(String(entry.type))) {
    throw new Error(`MissingCreateStrategy(${entry.type})`);
  }
  if (entry.claim?.mode === 'exact') {
    const prior = EXACT_CLAIMS.get(entry.claim.kind);
    if (prior && prior.type !== entry.type) {
      throw new Error(`DuplicateExactClaim(${entry.claim.kind})`);
    }
  }
  if (entry.claim?.mode === 'prefix') {
    const prior = PREFIX_CLAIMS.get(entry.claim.prefix);
    if (prior && prior.type !== entry.type) {
      throw new Error(`DuplicatePrefixClaim(${entry.claim.prefix})`);
    }
  }
  if (entry.refreshBacking === 'controller' && !entry.createController) {
    throw new Error(`RefreshBackingMissingController(${entry.type})`);
  }
  // The cast is the price of letting one Map hold heterogeneous entries.
  // Callers see the typed `CardEntry<T>`; the map stores the erased shape.
  const erased = entry as unknown as CardEntry<WaveCardData>;
  REGISTRY.set(entry.type, erased);
  if (entry.claim?.mode === 'exact') EXACT_CLAIMS.set(entry.claim.kind, erased);
  if (entry.claim?.mode === 'prefix') PREFIX_CLAIMS.set(entry.claim.prefix, erased);
}

export function getEntry(type: string): CardEntry<WaveCardData> | undefined {
  return REGISTRY.get(type);
}

export function __resetRegistryForTest(): void {
  REGISTRY.clear();
  EXACT_CLAIMS.clear();
  PREFIX_CLAIMS.clear();
}

export function renderCard(
  card: WaveCardData,
  opts: { onClose?: () => void; deletable?: boolean } = {},
): ReactNode {
  const entry = REGISTRY.get(card.type);
  if (!entry) {
    warnOnce(`render:${card.type}`, `[cards] no registry entry for type "${card.type}"`);
    return null;
  }
  // The map's value type is widened; each Component's prop type was specific
  // when registered, but at the call site we only know `WaveCardData`.
  // The discriminator (`card.type === entry.type`) guarantees runtime
  // alignment with the entry's Component prop type. createElement (not JSX)
  // so this file stays a plain .ts module — keeps the design-doc filename.
  return createElement(
    CardInstanceProvider,
    {
      cardId: card.id ?? card.type,
      deletable: opts.deletable !== false,
      card,
    },
    createElement(entry.Component as FC<CardComponentProps>, {
      card,
      onClose: opts.onClose,
      deletable: opts.deletable,
    }),
  );
}

export function sizeFor(card: WaveCardData): CardSize {
  const entry = REGISTRY.get(card.type);
  if (!entry) {
    warnOnce(`size:${card.type}`, `[cards] no registry entry for type "${card.type}" — using fallback size`);
    return FALLBACK_SIZE;
  }
  return entry.defaultSize;
}

export interface AddPanelMenuItem {
  type: string;
  label: string;
  /** Optional create-form schema. The menu host shows the inline config
   *  card if this is set; otherwise the kind is created immediately. */
  createSchema?: CreateSchema;
}

/** Entries that opted into the AddPanel menu. */
export function addPanelEntries(): AddPanelMenuItem[] {
  const out: AddPanelMenuItem[] = [];
  for (const entry of REGISTRY.values()) {
    if (
      entry.addPanel &&
      entry.create?.mode !== 'catalog' &&
      entry.create?.mode !== 'kernel-minted-only'
    ) {
      out.push({
        type: String(entry.type),
        label: entry.addPanel.label,
        createSchema: entry.addPanel.createSchema,
      });
    }
  }
  return out;
}

/** Kernel-card → UI-card adapter. Iterates registry entries with a
 *  `fromKernel` adapter and returns the first non-null match.
 *
 *  Plugin cards (kind starts with `ui://`) are caught by
 *  `PluginIframeEntry.fromKernel`, which emits the `'plugin'` discriminator.
 *  Only `ui://` is accepted; the legacy `plugin:` form was deleted in M4.
 *  The actual AppBridge mount + tool call wiring is the M5 full-integration
 *  concern.
 */
export function adaptKernelCard(k: KernelCard): WaveCardData | null {
  const exact = EXACT_CLAIMS.get(k.kind);
  if (exact?.fromKernel) {
    const adapted = exact.fromKernel(k);
    if (adapted) return adapted;
  }
  let prefixEntry: CardEntry<WaveCardData> | null = null;
  let prefixLen = -1;
  for (const [prefix, entry] of PREFIX_CLAIMS) {
    if (k.kind.startsWith(prefix) && prefix.length > prefixLen) {
      prefixEntry = entry;
      prefixLen = prefix.length;
    }
  }
  if (prefixEntry?.fromKernel) {
    const adapted = prefixEntry.fromKernel(k);
    if (adapted) return adapted;
  }
  for (const entry of REGISTRY.values()) {
    if (!entry.fromKernel) continue;
    if (entry === exact || entry === prefixEntry) continue;
    const adapted = entry.fromKernel(k);
    if (adapted) return adapted;
  }
  return null;
}

const CardInstanceReactCtx = createContext<CardInstanceCtx | null>(null);
const CardLifecycleReactCtx = createContext<CardLifecycleStore | null>(null);

export function CardInstanceProvider({
  cardId,
  deletable,
  card,
  children,
}: {
  cardId: string;
  deletable: boolean;
  card?: WaveCardData;
  children?: ReactNode;
}) {
  const slots = useRef(new Map<string, unknown>());
  const [, setVersion] = useState(0);
  const lifecycleWriter = useMemo(() => createCardLifecycleStore(), []);
  const lifecycleStore = useMemo(
    () =>
      Object.freeze({
        getSnapshot: lifecycleWriter.getSnapshot,
        subscribe: lifecycleWriter.subscribe,
      }),
    [lifecycleWriter],
  );
  const emit = useMemo(
    () => (cmd: CardRuntimeCommand) => {
      if (cmd.type === 'refresh') lifecycleWriter.bumpRefresh();
    },
    [lifecycleWriter],
  );

  const ctx: CardInstanceCtx = {
    cardId,
    deletable,
    emit,
    useInstance<S>(key: string, initial: S) {
      if (!slots.current.has(key)) slots.current.set(key, initial);
      const value = slots.current.get(key) as S;
      const setValue: Dispatch<SetStateAction<S>> = (next) => {
        const current = slots.current.get(key) as S;
        const resolved =
          typeof next === 'function'
            ? (next as (prev: S) => S)(current)
            : next;
        slots.current.set(key, resolved);
        setVersion((version) => version + 1);
      };
      return [value, setValue];
    },
  };
  const controllerRef = useRef<CardController | null>(null);
  const controllerInputRef = useRef({
    card,
    entry: card ? getEntry(card.type) : undefined,
    cardId,
    deletable,
    ctx,
    emit,
    lifecycleStore,
  });
  controllerInputRef.current = {
    card,
    entry: card ? getEntry(card.type) : undefined,
    cardId,
    deletable,
    ctx,
    emit,
    lifecycleStore,
  };
  const prevLifecycleRef = useRef<CardLifecycleSnapshot>(
    lifecycleWriter.getSnapshot(),
  );

  useEffect(() => {
    // PR4 will add IntersectionObserver-driven visibility tests.
    const current = controllerInputRef.current;
    if (!current.card || !current.entry?.createController) return;
    const controller = current.entry.createController({
      card: current.card,
      lifecycle: current.lifecycleStore,
      instance: {
        cardId: current.cardId,
        deletable: current.deletable,
        useInstance: current.ctx.useInstance,
      },
      emit: current.emit,
    });
    if (
      current.entry.refreshBacking === 'epoch' &&
      controller.onRefresh != null
    ) {
      throw new Error(
        'RefreshBackingConflict(' +
          current.entry.type +
          '): refreshBacking=epoch forbids controller.onRefresh; use refreshBacking=controller or remove onRefresh.',
      );
    }
    controllerRef.current = controller;
    return () => {
      const c = controllerRef.current;
      controllerRef.current = null;
      void c?.dispose?.();
    };
  }, [cardId]);

  useEffect(() => {
    return lifecycleWriter.subscribe(() => {
      const current = lifecycleWriter.getSnapshot();
      const prev = prevLifecycleRef.current;
      const controller = controllerRef.current;
      if (current.visible !== prev.visible) {
        void controller?.onVisibleChange?.(current.visible);
      }
      if (current.focused !== prev.focused) {
        void controller?.onFocusChange?.(current.focused);
      }
      if (!sameGeometry(current.geometry, prev.geometry)) {
        void controller?.onResize?.(current.geometry);
      }
      if (current.refreshEpoch > prev.refreshEpoch) {
        void controller?.onRefresh?.();
      }
      prevLifecycleRef.current = current;
    });
  }, [lifecycleWriter]);

  return createElement(
    CardInstanceReactCtx.Provider,
    { value: ctx },
    createElement(CardLifecycleReactCtx.Provider, { value: lifecycleStore }, children),
  );
}

export function useCardInstanceCtx(): CardInstanceCtx {
  const ctx = useContext(CardInstanceReactCtx);
  if (!ctx) throw new Error('useCardInstanceCtx outside CardInstanceProvider');
  return ctx;
}

export function useCardLifecycle(): CardLifecycleStore {
  const s = useContext(CardLifecycleReactCtx);
  if (!s) throw new Error('useCardLifecycle outside CardInstanceProvider');
  return s;
}

export function useOptionalCardInstanceCtx(): CardInstanceCtx | null {
  return useContext(CardInstanceReactCtx);
}

export function useInstanceValue<S>(key: string, initial: S): S {
  return useCardInstanceCtx().useInstance<S>(key, initial)[0];
}

export function useInstanceSetter<S>(
  key: string,
  initial: S,
): Dispatch<SetStateAction<S>> {
  return useCardInstanceCtx().useInstance<S>(key, initial)[1];
}
