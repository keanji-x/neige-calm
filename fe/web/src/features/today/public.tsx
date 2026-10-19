import { EmptyState } from '../../ui/empty-state/public.tsx';
import { Icon } from '../../ui/icon/public.tsx';
import styles from './today.module.css';

export function TodayReportEmpty() {
  return <section className={styles.empty} aria-label="空白日报"><EmptyState icon={<Icon name="tasks" />} title="今天还没有记录" description="说说今天想推进的事，计划与进展会整理在这里。" /></section>;
}
