import { useCardOverlay } from './useCardOverlay';

export interface StatusOverlayPayload {
  state: string;
}

export interface ProgressOverlayPayload {
  value: number;
}

export interface TextOverlayPayload {
  text: string;
}

export interface LayoutOverlayPayload {
  schemaVersion?: number;
  cards: unknown[];
}

export interface FileViewerNavOverlayPayload {
  schemaVersion?: number;
  tab: 'code' | 'diff';
  folderPath: string;
  selectedPath?: string | null;
  diffSelected?: string | null;
}

export interface AnyCardNeedsInputOverlayPayload {
  schemaVersion?: number;
  value: boolean;
}

export interface OverlayKindPayloadRegistry {
  status: StatusOverlayPayload;
  progress: ProgressOverlayPayload;
  eta: TextOverlayPayload;
  now: TextOverlayPayload;
  layout: LayoutOverlayPayload;
  'file-viewer-nav': FileViewerNavOverlayPayload;
  any_card_needs_input: AnyCardNeedsInputOverlayPayload;
}

export const kernelOverlayKinds = [
  'status',
  'progress',
  'eta',
  'now',
  'layout',
  'file-viewer-nav',
  'any_card_needs_input',
] as const satisfies readonly (keyof OverlayKindPayloadRegistry)[];

export type KernelOverlayKind = typeof kernelOverlayKinds[number];
export type OverlayKind = keyof OverlayKindPayloadRegistry & string;
export type OverlayPayload<K extends string> = K extends KernelOverlayKind
  ? OverlayKindPayloadRegistry[K]
  : unknown;

export function isKernelOverlayKind(kind: string): kind is KernelOverlayKind {
  return (kernelOverlayKinds as readonly string[]).includes(kind);
}

export function useTypedCardOverlay<K extends string>(
  cardId: string | undefined,
  overlayKind: K,
): OverlayPayload<K> | null {
  return useCardOverlay<OverlayPayload<K>>(cardId, overlayKind);
}
