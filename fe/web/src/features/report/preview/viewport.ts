// The preview block's simulated viewport (#1780): which device the frame pretends to be, and the
// scale that fits that device's true CSS-pixel box into the report's column. A per-reader view
// preference, remembered through the app's display preferences — never part of the block's payload.

export type PreviewPresetId = 'fit' | 'iphone-15' | 'pixel-8' | 'ipad' | 'laptop' | 'desktop' | 'custom';

export type ViewportSize = Readonly<{ width: number; height: number }>;

export type PreviewPreset = Readonly<{ id: PreviewPresetId; label: string; size: ViewportSize | null }>;

/** `size: null` is not a device: `fit` takes the column, `custom` takes the reader's numbers. */
export const PREVIEW_PRESETS: readonly PreviewPreset[] = Object.freeze([
  Object.freeze({ id: 'fit', label: 'Fit', size: null }),
  Object.freeze({ id: 'iphone-15', label: 'iPhone 15', size: Object.freeze({ width: 393, height: 852 }) }),
  Object.freeze({ id: 'pixel-8', label: 'Pixel 8', size: Object.freeze({ width: 412, height: 915 }) }),
  Object.freeze({ id: 'ipad', label: 'iPad', size: Object.freeze({ width: 820, height: 1180 }) }),
  Object.freeze({ id: 'laptop', label: 'Laptop', size: Object.freeze({ width: 1280, height: 800 }) }),
  Object.freeze({ id: 'desktop', label: 'Desktop', size: Object.freeze({ width: 1440, height: 900 }) }),
  Object.freeze({ id: 'custom', label: 'Custom', size: null }),
] as const);

export const CUSTOM_MIN_PX = 240;
export const CUSTOM_MAX_PX = 3840;

/**
 * What the reader picked. `rotated` turns a device preset sideways; a custom size is rotated by
 * swapping its own numbers, so what the inputs show is always what the frame is.
 */
export type ViewportChoice = Readonly<{ preset: PreviewPresetId; custom: ViewportSize; rotated: boolean }>;

export const DEFAULT_VIEWPORT_CHOICE: ViewportChoice = Object.freeze({
  preset: 'fit',
  custom: Object.freeze({ width: 1024, height: 768 }),
  rotated: false,
});

export function clampCustomPx(value: number): number {
  return Math.round(Math.min(CUSTOM_MAX_PX, Math.max(CUSTOM_MIN_PX, value)));
}

/** The frame's true CSS-pixel box, or `null` for `fit` (the column's width, the payload's height). */
export function viewportSize(choice: ViewportChoice): ViewportSize | null {
  if (choice.preset === 'custom') return choice.custom;
  const size = PREVIEW_PRESETS.find((preset) => preset.id === choice.preset)?.size ?? null;
  if (size === null || !choice.rotated) return size;
  return { width: size.height, height: size.width };
}

/**
 * Shrink only, never enlarge: the whole device fits both the available width and the stage's
 * height. An unmeasured width (`null`, `0`) does not constrain.
 */
export function viewportScale(size: ViewportSize, availableWidth: number | null, maxHeight: number): number {
  const byWidth = availableWidth !== null && availableWidth > 0 ? availableWidth / size.width : 1;
  const byHeight = maxHeight > 0 ? maxHeight / size.height : 1;
  return Math.min(1, byWidth, byHeight);
}

/**
 * Where a report's preview blocks remember the reader's device choice, by block `key`. The app
 * scopes it (per track) and owns the storage; either call may throw and neither may cost the frame.
 */
export type PreviewViewportStore = Readonly<{
  read: (key: string) => string | null;
  write: (key: string, value: string) => void;
}>;

function isPresetId(value: unknown): value is PreviewPresetId {
  return PREVIEW_PRESETS.some((preset) => preset.id === value);
}

function readSize(value: unknown): ViewportSize | null {
  if (typeof value !== 'object' || value === null) return null;
  const { width, height } = value as Record<string, unknown>;
  if (typeof width !== 'number' || typeof height !== 'number' || !Number.isFinite(width) || !Number.isFinite(height)) {
    return null;
  }
  return { width: clampCustomPx(width), height: clampCustomPx(height) };
}

/** Anything unreadable — no store, a throwing store, a stale or foreign shape — is the default. */
export function readViewportChoice(store: PreviewViewportStore | undefined, key: string): ViewportChoice {
  if (store === undefined) return DEFAULT_VIEWPORT_CHOICE;
  try {
    const raw = store.read(key);
    if (raw === null) return DEFAULT_VIEWPORT_CHOICE;
    const parsed: unknown = JSON.parse(raw);
    if (typeof parsed !== 'object' || parsed === null) return DEFAULT_VIEWPORT_CHOICE;
    const { preset, custom, rotated } = parsed as Record<string, unknown>;
    return {
      preset: isPresetId(preset) ? preset : DEFAULT_VIEWPORT_CHOICE.preset,
      custom: readSize(custom) ?? DEFAULT_VIEWPORT_CHOICE.custom,
      rotated: rotated === true,
    };
  } catch {
    return DEFAULT_VIEWPORT_CHOICE;
  }
}

/** Best effort: a browser that refuses storage still previews, it just forgets. */
export function writeViewportChoice(store: PreviewViewportStore | undefined, key: string, choice: ViewportChoice): void {
  if (store === undefined) return;
  try {
    store.write(key, JSON.stringify(choice));
  } catch {
    // Remembering is a convenience here; nothing to recover.
  }
}
