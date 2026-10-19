import { Icon } from '../../../ui/icon/public.tsx';
import { EmptyState } from '../../../ui/empty-state/public.tsx';
import styles from './empty.module.css';

export function ReportEmpty() {
  return <div className={styles.empty} data-nc-report-empty="">
    <EmptyState align="center" icon={<Icon name="file" />} title="进展会整理在这里"
      description="说说想做的事，计划与结论会随对话整理。" />
  </div>;
}
