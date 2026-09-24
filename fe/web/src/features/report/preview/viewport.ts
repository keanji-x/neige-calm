// The preview block's simulated viewport (#1780): which device the frame pretends to be, and the
// scale that fits that device's true CSS-pixel box into the report's column. A per-reader view
// preference, remembered through the app's display preferences — never part of the block's payload.

export type PreviewPresetId = 'desktop' | 'mobile';

export type ViewportSize = Readonly<{ width: number; height: number }>;

export type PreviewPreset = Readonly<{ id: PreviewPresetId; label: string; size: ViewportSize }>;

export const PREVIEW_PRESETS: readonly PreviewPreset[] = Object.freeze([
  Object.freeze({ id: 'desktop', label: 'Desktop', size: Object.freeze({ width: 1920, height: 1080 }) }),
  Object.freeze({ id: 'mobile', label: 'Mobile', size: Object.freeze({ width: 390, height: 844 }) }),
] as const);

export const DEFAULT_PRESET: PreviewPresetId = 'desktop';

export function presetSize(id: PreviewPresetId): ViewportSize {
  return (PREVIEW_PRESETS.find((preset) => preset.id === id) ?? PREVIEW_PRESETS[0]).size;
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

/** Anything else — no store, a throwing store, a value from an older shape — is Desktop. */
export function readPreset(store: PreviewViewportStore | undefined, key: string): PreviewPresetId {
  if (store === undefined) return DEFAULT_PRESET;
  try {
    const raw = store.read(key);
    return PREVIEW_PRESETS.find((preset) => preset.id === raw)?.id ?? DEFAULT_PRESET;
  } catch {
    return DEFAULT_PRESET;
  }
}

/** Best effort: a browser that refuses storage still previews, it just forgets. */
export function writePreset(store: PreviewViewportStore | undefined, key: string, preset: PreviewPresetId): void {
  if (store === undefined) return;
  try {
    store.write(key, preset);
  } catch {
    // Remembering is a convenience here; nothing to recover.
  }
}
