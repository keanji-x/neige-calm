import { ListText } from '../../../ui/list-typography/public.tsx';
import { ContextItem } from '../../../ui/panel-card/context.tsx';
import { PanelModule } from '../../../ui/panel-card/public.tsx';
import styles from './recent-files.module.css';

function basename(path: string): string {
  return path.slice(path.lastIndexOf('/') + 1);
}

export function RecentFiles({ paths, onOpen, inline = false }: Readonly<{
  paths: readonly string[];
  inline?: boolean;
  onOpen: (path: string) => void;
}>) {
  if (paths.length === 0) return null;
  const list = (
      <ul className={`${styles.list} ${inline ? styles.context : ''}`} data-nc-recent-files="" aria-label={inline ? "Recently opened files" : undefined}>
        {paths.map((path) => (
          <li key={path} className={styles.item}>
            {inline ? <ContextItem icon="file" label={basename(path)} trailing={<ListText tone="secondary">Recent</ListText>} buttonProps={{ 'aria-label': `Open ${path}`,
              title: `Recently opened: ${path}`, onClick: () => onOpen(path) }} /> : <button
              type="button"
              className={styles.row}
              aria-label={`Open ${path}`}
              title={path}
              onClick={() => onOpen(path)}
            >
              <span className={styles.name}>{basename(path)}</span>
            </button>}
          </li>
        ))}
      </ul>
  );
  return inline ? list : <PanelModule title="Recent files" icon="file">{list}</PanelModule>;
}
