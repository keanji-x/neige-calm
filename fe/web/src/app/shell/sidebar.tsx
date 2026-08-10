// The workspace rail. Three sections in a fixed order; see INV-SIDEBAR-007.

import { coveOf, type Cove } from '../../../../core/domain/cove.ts';
import {
  isRunning, isWaitingForUser, waveDisplayTitle, type Wave,
} from '../../../../core/domain/wave.ts';
import { useTheme } from '../theme/public.tsx';
import type { NavTarget } from '../router/navigation.ts';
import styles from './shell.module.css';

const NEXT_THEME_MODE = Object.freeze({ system: 'light', light: 'dark', dark: 'system' } as const);

export type SidebarProps = Readonly<{
  coves: readonly Cove[];
  wavesByCove: ReadonlyMap<string, readonly Wave[]>;
  waves: readonly Wave[];
  currentPath: string;
  onGo: (target: NavTarget) => void;
}>;

/**
 * INV-SIDEBAR-007 — the three sections render in this order, and **pinning is
 * not relocation**: a pinned wave appears in the Pinned section *and* in its
 * cove's inline list, and if it also needs attention it appears in "Waiting on
 * you" as well. Waiting deliberately includes pinned attention waves.
 *
 * INV-A11Y-058 — there is **intentionally no skip-to-main link**. The rail is
 * short enough that it has not been raised as a pain point (a11y contract
 * §3.1/§9). If a second rail section with many rows lands, re-evaluate; until
 * then "there is no skip link" is a decision, not a defect.
 *
 * INV-SIDEBAR-012 (pin button visible on hover, always visible once pinned) is
 * **not** in this slice: the rail is read-only until wave mutations land, so
 * there is no pin affordance to hide or reveal yet.
 */
export function Sidebar({ coves, wavesByCove, waves, currentPath, onGo }: SidebarProps) {
  const { mode, resolved, setMode } = useTheme();
  const waiting = waves.filter((wave) => isWaitingForUser(wave.lifecycle));
  const pinned = waves.filter((wave) => wave.pinnedAt !== null)
    .toSorted((left, right) => (right.pinnedAt ?? 0) - (left.pinnedAt ?? 0));

  return (
    <nav className={styles.rail} aria-label="Workspace">
      <div className={styles.brandRow}>
        <button type="button" className={styles.brand} onClick={() => onGo({ name: 'today' })}>
          neige · calm
        </button>
        <button
          type="button"
          className={styles.themeToggle}
          aria-label={`Theme: ${mode} (resolved ${resolved})`}
          onClick={() => setMode(NEXT_THEME_MODE[mode])}
        >
          {mode}
        </button>
      </div>

      <WaveSection title="Waiting on you" waves={waiting} coves={coves} currentPath={currentPath} onGo={onGo} />
      <WaveSection title="Pinned" waves={pinned} coves={coves} currentPath={currentPath} onGo={onGo} />

      <div className={styles.section}>
        <h2 className={styles.sectionTitle}>Coves</h2>
        {coves.length === 0 && <p className={styles.empty}>No coves yet.</p>}
        {coves.map((cove) => {
          const coveWaves = wavesByCove.get(cove.id) ?? [];
          const active = currentPath === `/cove/${cove.id}`;
          return (
            <div key={cove.id}>
              <button
                type="button"
                className={`${styles.coveRow} ${active ? styles.rowActive : ''}`}
                aria-current={active ? 'page' : undefined}
                onClick={() => onGo({ name: 'cove', coveId: cove.id })}
              >
                <span className={styles.swatch} style={{ background: cove.color }} aria-hidden="true" />
                <span className={styles.rowLabel}>{cove.name}</span>
                <span className={styles.count}>{coveWaves.length}</span>
              </button>
              <div className={styles.waveList}>
                {coveWaves.map((wave) => (
                  <WaveRow key={wave.id} wave={wave} currentPath={currentPath} onGo={onGo} />
                ))}
              </div>
            </div>
          );
        })}
      </div>
    </nav>
  );
}

function WaveSection({ title, waves, coves, currentPath, onGo }: {
  title: string;
  waves: readonly Wave[];
  coves: readonly Cove[];
  currentPath: string;
  onGo: (target: NavTarget) => void;
}) {
  if (waves.length === 0) return null;
  return (
    <div className={styles.section}>
      <h2 className={styles.sectionTitle}>{title}</h2>
      {waves.map((wave) => (
        <WaveRow
          key={wave.id}
          wave={wave}
          coveName={coveOf(wave.coveId, coves)?.name}
          currentPath={currentPath}
          onGo={onGo}
        />
      ))}
    </div>
  );
}

/** INV-A11Y-061 — navigation is `<button>` + `onGo`, never a native `<a href>`. */
function WaveRow({ wave, coveName, currentPath, onGo }: {
  wave: Wave;
  coveName?: string;
  currentPath: string;
  onGo: (target: NavTarget) => void;
}) {
  const waiting = isWaitingForUser(wave.lifecycle);
  const running = isRunning(wave.lifecycle);
  const title = waveDisplayTitle(wave.title);
  const active = currentPath === `/wave/${wave.id}`;
  const flag = waiting ? styles.waveFlagWaiting : running ? styles.waveFlagRunning : '';
  return (
    <button
      type="button"
      className={`${styles.waveRow} ${active ? styles.rowActive : ''}`}
      aria-current={active ? 'page' : undefined}
      aria-label={coveName === undefined ? `Wave ${title}` : `Wave ${title}, in cove ${coveName}`}
      onClick={() => onGo({ name: 'wave', waveId: wave.id })}
    >
      <span className={`${styles.waveFlag} ${flag}`} aria-hidden="true" />
      <span className={styles.rowLabel}>{title}</span>
    </button>
  );
}
