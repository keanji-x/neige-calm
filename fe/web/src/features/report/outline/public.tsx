import type { ReportOutlineItem } from '../../../../../core/domain/report.ts';
import { EdgeNavigator } from '../../../ui/edge-navigation/public.tsx';
import { revealReportAnchor } from '../anchor/public.ts';
import styles from './outline.module.css';

export type ReportOutlineProps = Readonly<{
  items: readonly ReportOutlineItem[];
  onSelect?: (anchorId: string) => void;
}>;
const revealOutlineAnchor = (anchorId: string) => revealReportAnchor(anchorId, document, 'smooth');

/** The report owns chapter IDs and placement; navigation behavior is shared with chat. */
export function ReportOutline({ items, onSelect = revealOutlineAnchor }: ReportOutlineProps) {
  if (items.length === 0) return null;
  return <nav className={styles.rail} aria-label="Outline" data-nc-report-outline="">
    <div className={styles.viewport}><div className={styles.navigator}>
      <EdgeNavigator label="Jump to a section" activeId={null} onSelect={onSelect}
        items={items.map(item => ({ id: item.blockId, text: item.label, label: item.label }))} />
    </div></div>
  </nav>;
}
