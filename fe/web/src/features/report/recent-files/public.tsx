import { PanelModule } from '../../../ui/panel-card/public.tsx';
import styles from './recent-files.module.css';

function basename(path: string): string {
  return path.slice(path.lastIndexOf('/') + 1);
}

export function RecentFiles({ paths, onOpen }: Readonly<{
  paths: readonly string[];
  onOpen: (path: string) => void;
}>) {
  if (paths.length === 0) return null;
  return (
    <PanelModule title="Recent files">
      <ul className={styles.list} data-nc-recent-files="">
        {paths.map((path) => (
          <li key={path} className={styles.item}>
            <button
              type="button"
              className={styles.row}
              aria-label={`Open ${path}`}
              title={path}
              onClick={() => onOpen(path)}
            >
              <span className={styles.name}>{basename(path)}</span>
            </button>
          </li>
        ))}
      </ul>
    </PanelModule>
  );
}
