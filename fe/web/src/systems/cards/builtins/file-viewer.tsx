// The file card. Kernel kind `'file-viewer'`, payload `{ path }`. Owns a surface; owns no runtime, so its create is `generic`.
// `buildPayload` takes only `path`: `title` is a column on the row, not a member of the payload.

import type { CardComponentProps, CardEntry, KernelCardInput } from '../registry.js';
import { FileViewer } from '../../fs-viewers/public.tsx';
import { readHostTheme } from '../../fs-viewers/theme.ts';
import { CardHead } from '../ui/card-head.tsx';

declare module '../registry.js' {
  interface CardDataMap {
    'file-viewer': FileViewerCard;
  }
}

export type FileViewerCard = Readonly<{
  type: 'file-viewer';
  id: string;
  title: string | null;
  /** Absolute path to the file or folder the card opens at. */
  path: string;
}>;

/** A card with no readable `path` resolves to `null` and lands in the board's `unknown` branch, where a reader can at least delete it. */
function pathFromPayload(payload: unknown): string | null {
  if (typeof payload !== 'object' || payload === null) return null;
  const value = (payload as { path?: unknown }).path;
  return typeof value === 'string' && value !== '' ? value : null;
}

function FileViewerCardView({ card, host, onRemove }: CardComponentProps<FileViewerCard>) {
  return (
    <div className="term fv-card" data-nc-file-card="">
      <CardHead
        className="card-drag-handle"
        title={card.title ?? 'file'}
        onClose={onRemove}
        closeAriaLabel={`Delete card ${card.title ?? 'file'}`}
      />
      {/* The theme is read at render from `<html data-theme>` rather than subscribed to: `systems/**` may not reach `app/theme`. A toggle while a card is open repaints on the next render. */}
      <FileViewer path={card.path} files={host.files} theme={readHostTheme()} slots={host.slots} />
    </div>
  );
}

export const FILE_VIEWER_CARD_ENTRY = Object.freeze({
  type: 'file-viewer',
  component: (props: CardComponentProps<FileViewerCard>) => FileViewerCardView(props),
  headless: false,
  defaultSize: Object.freeze({ w: 6, h: 12, minW: 4, minH: 6 }),
  claim: Object.freeze({ mode: 'exact', kind: 'file-viewer' } as const),
  title: (card: FileViewerCard) => card.title ?? 'file',
  accessibleName: (card: FileViewerCard) => `File: ${card.path}`,
  create: Object.freeze({
    mode: 'generic' as const,
    buildPayload: (input: Readonly<Record<string, string>>) => ({ path: input.path ?? '' }),
  }),
  addPanel: Object.freeze({
    label: 'file',
    fields: Object.freeze([
      Object.freeze({ key: 'title', label: 'Title', kind: 'text' as const, placeholder: 'file' }),
      Object.freeze({
        key: 'path',
        label: 'File or folder',
        kind: 'file' as const,
        required: true,
        placeholder: 'Choose a file or folder…',
        hint: 'A folder opens its listing; a file opens with its folder beside it.',
      }),
    ]),
  }),
  fromKernel: (card: KernelCardInput): FileViewerCard | null => {
    if (card.kind !== 'file-viewer') return null;
    const path = pathFromPayload(card.payload);
    return path === null
      ? null
      : Object.freeze({ type: 'file-viewer', id: card.id, title: null, path } as const);
  },
}) satisfies CardEntry<FileViewerCard>;
