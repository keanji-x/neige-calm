import { readContentSelection } from '../../../ui/drawer/selection.ts';
import { Component, type ReactNode, type RefObject } from 'react';
import { withScrollRestoration } from '../../../ui/drawer/follow-scroll.ts';
import { readingPlaceIn, restoreReadingPlace, type ReadingPlace } from '../../../ui/drawer/reading-place.ts';
import type { TranscriptEntry } from '../../../../../core/domain/conversation.ts';

type Props = Readonly<{
  frame: RefObject<HTMLDivElement | null>;
  turns: readonly TranscriptEntry[];
  children: ReactNode;
}>;
type Snapshot = Readonly<{ pane: HTMLElement; place: ReadingPlace }> | null;

/** React's before-mutation snapshot is needed here: effect cleanup already sees
 * the prepended DOM. Keep the existing native text mark and restoration fence. */
export class PrependReading extends Component<Props, Record<string, never>, Snapshot> {
  getSnapshotBeforeUpdate(previous: Props): Snapshot {
    const first = previous.turns[0];
    if (first === undefined || this.props.turns[0]?.id === first.id
      || !this.props.turns.some(turn => turn.id === first.id)) return null;
    const frame = this.props.frame.current;
    const pane = frame?.closest<HTMLElement>('[data-nc-drawer-scroll]');
    if (pane == null || frame === null) return null;
    const place = readingPlaceIn(pane);
    const selected = readContentSelection(frame).length > 0;
    return { pane, place: selected ? { ...place, atEnd: false } : place };
  }

  componentDidUpdate(_previous: Props, _state: Record<string, never>, snapshot: Snapshot): void {
    if (snapshot === null) return;
    withScrollRestoration([snapshot.pane], () => restoreReadingPlace(snapshot.pane, snapshot.place));
  }

  render(): ReactNode { return this.props.children; }
}
