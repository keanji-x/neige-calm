import type { DialogOrigin } from '../../../ui/dialog/public.tsx';
import { GuideAnchor } from '../../../ui/floating-control/public.ts';
import { useEffect, useLayoutEffect, useRef, type ComponentProps } from 'react';
import { flushSync } from 'react-dom';
import { Button } from '@astryxdesign/core/Button';
import { Popover } from '@astryxdesign/core/Popover';
import { Icon } from '../../../ui/icon/public.tsx';
import { useState } from '../../../ui/state/public.ts';
import { useCompactViewport } from '../../../ui/viewport/public.ts';
import { FolderPill, StartingPointPill, IsolationPill } from '../default-pills/public.tsx';
import styles from './composer-preferences.module.css';

type Props = Readonly<{
  startingPoint: ComponentProps<typeof StartingPointPill>;
  folder: Omit<ComponentProps<typeof FolderPill>, 'onBrowse'> & { onBrowse: (origin: DialogOrigin) => void };
  browsing: boolean;
  isolation: ComponentProps<typeof IsolationPill>;
}>;

/** Button owns truncation. Include its clipped text when measuring the space
 * the real control needs, without mounting a second picker or copying IDs. */
function fullButtonWidth(button: HTMLButtonElement): number {
  const label = [...button.querySelectorAll('span')]
    .find((node) => getComputedStyle(node).textOverflow === 'ellipsis');
  // Popover opening transforms must not change the intrinsic layout budget.
  return button.offsetWidth
    + (label === undefined ? 0 : Math.max(0, label.scrollWidth - label.clientWidth));
}

export function ComposerPreferences({ startingPoint, folder, browsing, isolation }: Props) {
  const compact = useCompactViewport();
  const [collapsed, setCollapsed] = useState(false);
  const [open, setOpen] = useState(false);
  const hostRef = useRef<HTMLDivElement>(null);
  const itemsRef = useRef<HTMLDivElement>(null);
  const startingRef = useRef<HTMLSpanElement>(null);
  const folderRef = useRef<HTMLSpanElement>(null);
  const moreRef = useRef<HTMLButtonElement>(null);
  const isolationRef = useRef<HTMLDivElement>(null);
  const requiredWidth = useRef(0);
  const measuredLabels = useRef('');
  const restoreFocus = useRef(false);
  const wasBrowsing = useRef(browsing);

  useEffect(() => {
    const closed = wasBrowsing.current && !browsing;
    wasBrowsing.current = browsing;
    if (!closed) return;
    // Selection can remeasure/refold the controls in this commit. Restore the
    // resulting reachable control after Dialog cleanup, never its removed Folder.
    // Dialog's passive cleanup has already restored its opener. This one-frame
    // repair only owns the remaining refold transition, not a later focus choice.
    // focus() emits trusted focus events too: input trust cannot classify those.
    const release = () => {
      cancelAnimationFrame(frame);
      document.removeEventListener('pointerdown', yieldFocus, true);
      document.removeEventListener('keydown', yieldFocus, true);
      document.removeEventListener('focusin', yieldFocus, true);
    };
    const yieldFocus = (event: Event) => {
      if (event.type === 'focusin' ? event.target === document.activeElement : event.isTrusted) release();
    };
    const frame = requestAnimationFrame(() => {
      release();
      (moreRef.current ?? folderRef.current?.querySelector('button'))?.focus();
    });
    document.addEventListener('pointerdown', yieldFocus, true);
    document.addEventListener('keydown', yieldFocus, true);
    document.addEventListener('focusin', yieldFocus, true);
    return release;
  }, [browsing]);

  useLayoutEffect(() => {
    const host = hostRef.current;
    // Closed popovers unmount their content in Astryx 0.6.3. Keep observing
    // the footer and reuse its last measured control width until it opens.
    const items = itemsRef.current;
    // ChatComposer exposes footer slots, but not a ref for their shared row.
    const row = host?.parentElement?.parentElement;
    const actions = row?.lastElementChild;
    if (host === null || !(row instanceof HTMLElement) || !(actions instanceof HTMLElement)) return;
    const measure = () => {
      const template = startingRef.current?.querySelector('button');
      const folders = [...(folderRef.current?.querySelectorAll<HTMLButtonElement>(':scope > button') ?? [])];
      const buttons = template === null || template === undefined ? folders : [template, ...folders];
      const labels = buttons.map((button) => button.getAttribute('aria-label') ?? button.textContent).join('\n');
      if (items !== null && items.getBoundingClientRect().width > 0) {
        const gap = parseFloat(getComputedStyle(host).columnGap) || 0;
        requiredWidth.current = buttons.reduce((sum, button) => sum + fullButtonWidth(button), 0)
          + gap * Math.max(0, buttons.length - 1);
        measuredLabels.current = labels;
      } else if (items !== null && labels !== measuredLabels.current) {
        // A selection may change while its directory dialog hides the popover.
        // Measure the single live pair inline before the next paint, then fold
        // it again if necessary. Width never depends on the overflow trigger.
        setCollapsed(false);
        return;
      }
      const gap = parseFloat(getComputedStyle(row).columnGap) || 0;
      const isolationWidth = isolationRef.current?.getBoundingClientRect().width ?? 0;
      const available = row.clientWidth - actions.getBoundingClientRect().width - isolationWidth - gap * 2;
      const next = compact && requiredWidth.current > available + 0.5;
      if (next !== collapsed) {
        restoreFocus.current = (items?.contains(document.activeElement) ?? false) || moreRef.current === document.activeElement;
        setOpen(false);
        setCollapsed(next);
      }
    };
    if (restoreFocus.current) {
      restoreFocus.current = false;
      (collapsed ? moreRef.current : folderRef.current?.querySelector('button'))?.focus();
    }
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(row);
    observer.observe(actions);
    if (items !== null) observer.observe(items);
    if (isolationRef.current !== null) observer.observe(isolationRef.current);
    return () => observer.disconnect();
  });

  const preferences = <div ref={itemsRef} className={styles.items}>
    <span ref={startingRef} className={styles.startingPoint}><StartingPointPill {...startingPoint} /></span>
    <span ref={folderRef} className={styles.folder}><FolderPill {...folder} onBrowse={() => {
      // A native top-layer popup must relinquish hits and its focus trap before
      // the body-portal modal opens. Public controlled close also restores More.
      // Its Folder source becomes unavailable here: Motion's declared first-unavailable
      // policy centers this cycle; never cache a box or substitute a focus source.
      if (open) flushSync(() => setOpen(false));
      folder.onBrowse({ kind: 'anchor', anchor: { ref: folderRef }, unavailable: 'surface-center' });
    }} /></span>
  </div>;

  return <GuideAnchor id={collapsed ? 'track-options-collapsed' : 'track-options'} disabled={browsing || open} placement="top">{(anchor, describedBy) => <div ref={(node) => { hostRef.current = node; anchor(node); }} aria-describedby={describedBy} tabIndex={-1} className={styles.host} data-nc-composer-preferences>
    {collapsed ? <Popover label="Track options" placement="above" isOpen={open} onOpenChange={setOpen}
      content={<div className={styles.panel}>{preferences}</div>}>
      {(trigger) => <Button {...trigger} ref={(node) => { moreRef.current = node; trigger.ref(node); }}
        label="Track options" variant="secondary" className={styles.trigger} isIconOnly
        icon={<Icon name="more" />} isDisabled={startingPoint.isDisabled} />}
    </Popover> : preferences}
    <div ref={isolationRef} className={styles.isolation}><IsolationPill {...isolation} /></div>
  </div>}</GuideAnchor>;
}
