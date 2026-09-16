import { useEffect, useLayoutEffect, useRef, type ComponentProps } from 'react';
import { Button } from '@astryxdesign/core/Button';
import { Popover } from '@astryxdesign/core/Popover';
import { Icon } from '../../../ui/icon/public.tsx';
import { useState } from '../../../ui/state/public.ts';
import { useCompactViewport } from '../../../ui/viewport/public.ts';
import { FolderPill, StartingPointPill } from '../default-pills/public.tsx';
import styles from './composer-preferences.module.css';

type Props = Readonly<{
  startingPoint: ComponentProps<typeof StartingPointPill>;
  folder: ComponentProps<typeof FolderPill>;
  browsing: boolean;
}>;

/** Button owns truncation. Include its clipped text when measuring the space
 * the real control needs, without mounting a second picker or copying IDs. */
function fullButtonWidth(button: HTMLButtonElement): number {
  const label = [...button.querySelectorAll('span')]
    .find((node) => getComputedStyle(node).textOverflow === 'ellipsis');
  return button.getBoundingClientRect().width
    + (label === undefined ? 0 : Math.max(0, label.scrollWidth - label.clientWidth));
}

export function ComposerPreferences({ startingPoint, folder, browsing }: Props) {
  const compact = useCompactViewport();
  const [collapsed, setCollapsed] = useState(false);
  const [open, setOpen] = useState(false);
  const hostRef = useRef<HTMLDivElement>(null);
  const itemsRef = useRef<HTMLDivElement>(null);
  const startingRef = useRef<HTMLSpanElement>(null);
  const folderRef = useRef<HTMLSpanElement>(null);
  const moreRef = useRef<HTMLButtonElement>(null);
  const requiredWidth = useRef(0);
  const measuredLabels = useRef('');
  const restoreFocus = useRef(false);
  const wasBrowsing = useRef(browsing);

  useEffect(() => {
    const closed = wasBrowsing.current && !browsing;
    wasBrowsing.current = browsing;
    if (!closed) return;
    // A trusted click in the directory dialog can light-dismiss its parent
    // popover. Dialog then has a hidden Folder opener; restore the visible
    // context only if focus fell to the body, after its own cleanup completes.
    const frame = requestAnimationFrame(() => {
      if (document.activeElement !== document.body) return;
      (moreRef.current ?? folderRef.current?.querySelector('button'))?.focus();
    });
    return () => cancelAnimationFrame(frame);
  }, [browsing]);

  useLayoutEffect(() => {
    const host = hostRef.current;
    const items = itemsRef.current;
    // ChatComposer exposes footer slots, but not a ref for their shared row.
    const row = host?.parentElement?.parentElement;
    const actions = row?.lastElementChild;
    if (host === null || items === null || !(row instanceof HTMLElement) || !(actions instanceof HTMLElement)) return;
    const measure = () => {
      const template = startingRef.current?.querySelector('button');
      const folders = [...(folderRef.current?.querySelectorAll<HTMLButtonElement>(':scope > button') ?? [])];
      const buttons = template === null || template === undefined ? folders : [template, ...folders];
      const labels = buttons.map((button) => button.getAttribute('aria-label') ?? button.textContent).join('\n');
      if (items.getBoundingClientRect().width > 0) {
        const gap = parseFloat(getComputedStyle(host).columnGap) || 0;
        requiredWidth.current = buttons.reduce((sum, button) => sum + fullButtonWidth(button), 0)
          + gap * Math.max(0, buttons.length - 1);
        measuredLabels.current = labels;
      } else if (labels !== measuredLabels.current) {
        // A selection may change while its directory dialog hides the popover.
        // Measure the single live pair inline before the next paint, then fold
        // it again if necessary. Width never depends on the overflow trigger.
        setCollapsed(false);
        return;
      }
      const gap = parseFloat(getComputedStyle(row).columnGap) || 0;
      const available = row.clientWidth - actions.getBoundingClientRect().width - gap;
      const next = compact && requiredWidth.current > available + 0.5;
      if (next !== collapsed) {
        restoreFocus.current = items.contains(document.activeElement) || moreRef.current === document.activeElement;
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
    observer.observe(items);
    return () => observer.disconnect();
  });

  const preferences = <div ref={itemsRef} className={styles.items}>
    <span ref={startingRef} className={styles.startingPoint}><StartingPointPill {...startingPoint} /></span>
    <span ref={folderRef} className={styles.folder}><FolderPill {...folder} /></span>
  </div>;

  return <div ref={hostRef} className={styles.host} data-nc-composer-preferences>
    {collapsed ? <Popover label="Track options" placement="above" isOpen={open} onOpenChange={setOpen}
      content={<div className={styles.panel}>{preferences}</div>}>
      {(trigger) => <Button {...trigger} ref={(node) => { moreRef.current = node; trigger.ref(node); }}
        label="Track options" variant="secondary" className={styles.trigger} isIconOnly
        icon={<Icon name="more" />} isDisabled={startingPoint.isDisabled} />}
    </Popover> : preferences}
  </div>;
}
