import { useRef } from 'react';
import type { DialogOrigin } from '../../../ui/dialog/public.tsx';
// Track-owned menu; hosts supply persistence and navigation-free actions.
import { DropdownMenu, DropdownMenuItem, DropdownMenuDivider } from '@astryxdesign/core/DropdownMenu';
import { trackDisplayTitle, type Track } from '../../../../../core/domain/track.ts';
import { Icon } from '../../../ui/icon/public.tsx';
import { useState } from '../../../ui/state/public.ts';

export type TrackActionsProps = Readonly<{
  track: Track;
  areaPinned: boolean;
  onSetPinned: (id: string, pinned: boolean) => void;
  onSetAreaPinned: (id: string, pinned: boolean) => void;
  onMarkUnread: (id: string) => void;
  onDelete?: (id: string, origin: DialogOrigin) => void;
  className?: string;

}>;

export function TrackActions({ track, areaPinned, onSetPinned, onSetAreaPinned, onMarkUnread, onDelete, className }: TrackActionsProps) {
  const sourceRef = useRef<HTMLSpanElement | null>(null);
  const [menuOpen, setMenuOpen] = useState(false);
  return <span ref={sourceRef} style={{ display: 'inline-flex' }}><DropdownMenu isMenuOpen={menuOpen} onOpenChange={setMenuOpen} placement="below" alignment="end" button={{
    label: `Actions for track ${trackDisplayTitle(track.title)}`, icon: <Icon name="more" size="sm" />,
    isIconOnly: true, variant: 'ghost', size: 'sm', className,
  }}>
    {menuOpen && <>
      <DropdownMenuItem label={track.pinnedAt === null ? 'Pin globally' : 'Unpin globally'} onClick={() => onSetPinned(track.id, track.pinnedAt === null)} />
    <DropdownMenuItem label={areaPinned ? 'Unpin within area' : 'Pin within area'} onClick={() => onSetAreaPinned(track.id, !areaPinned)} />
    <DropdownMenuItem label="Mark as unread" onClick={() => onMarkUnread(track.id)} />
    {onDelete !== undefined && <><DropdownMenuDivider /><DropdownMenuItem label="Delete track" onClick={() => onDelete(track.id, { kind: 'anchor', anchor: { ref: sourceRef }, unavailable: 'surface-center' })} /></>}
    </>}
  </DropdownMenu></span>;
}
