// The directory browser: an editable absolute path that is also a combobox
// over the listing it names.
//
// ## What this revision is (#1228)
//
// Only the rendering. The props surface, the port, and every keyboard rule
// below are the frozen §6.7a contract and are untouched; what the first cut
// deliberately left undone was the visual layer — its class names were
// placeholders with no CSS behind them, so the control shipped as unstyled
// browser defaults inside an otherwise finished dialog.
//
// ## Built from `@astryxdesign/core`, with the listbox kept local
//
// astryx is this repo's component library and it owns everything here that is
// a plain control: the path field, the parent button, and the two actions. It
// does *not* own the option list. `List`/`Item` hard-code their roles, and
// this list is a `role="listbox"` driven by `aria-activedescendant` from an
// input that keeps DOM focus — the one shape astryx has no component for. So
// the rows stay local markup with a CSS module, and only the chrome around
// them is astryx.
//
// Two seams that follow from that choice, both deliberate:
//
//   * Every astryx `Button` renders its own always-present
//     `<VisuallyHidden role="status">` loading region (`Button.tsx`). Three
//     buttons therefore put three status nodes on this surface, so the
//     loading row cannot be found by `role="status"` alone any more — the
//     test names it by its text instead. The row is still a live region; it
//     is just no longer the only one.
//   * The path input's own font comes from astryx (`--font-family-body`). A
//     path is one of the two things `fe-design.md:869` allows mono in a field
//     for, so the module overrides it — which works without a specificity
//     fight because the `ui` layer sorts after `astryx` in `entry.css`.
//
// ## The parent button is new, and it is not a new behaviour
//
// `DirectoryListing.parent` has always been part of the frozen port and this
// component has always ignored it: going up meant editing the text by hand.
// The button issues exactly the `load(parent)` that typing the parent path
// and pressing Enter already issued, so it adds a pointer affordance to an
// existing navigation, not a new one.

import { useEffect, useId, useMemo, useRef, type KeyboardEvent } from 'react';
import { Banner } from '@astryxdesign/core/Banner';
import { Button } from '@astryxdesign/core/Button';
import { TextInput } from '@astryxdesign/core/TextInput';

import { Icon } from '../icon/public.tsx';
import { useState } from '../state/public.ts';
import styles from './directory-browser.module.css';

export type DirectoryMode = 'directory' | 'file';
export interface DirectoryEntry { name: string; path: string; isDirectory: boolean }
export interface DirectoryListing { path: string; parent: string | null; entries: readonly DirectoryEntry[] }
export type ListDirectory = (path?: string) => Promise<DirectoryListing>;
export interface DirectoryBrowserProps {
  listDirectory: ListDirectory; initialPath: string | null; onCancel: () => void; onSelect: (path: string) => void;
  mode?: DirectoryMode; selectLabel?: string;
}

export function normalizeDirectoryPath(path: string): string { return path.length > 1 ? path.replace(/\/+$/, '') : path; }
export function directoryInputValue(path: string): string { return path === '/' ? '/' : `${normalizeDirectoryPath(path)}/`; }
export function joinDirectoryPath(parent: string, name: string): string { return parent === '/' ? `/${name}` : `${normalizeDirectoryPath(parent)}/${name}`; }

export function DirectoryBrowser({ listDirectory, initialPath, onCancel, onSelect, mode = 'directory', selectLabel = mode === 'file' ? 'Select current folder' : 'Select this directory' }: DirectoryBrowserProps) {
  const [listing, setListing] = useState<DirectoryListing | null>(null);
  const [pathText, setPathText] = useState(initialPath ? directoryInputValue(initialPath) : '');
  const [activeIndex, setActiveIndex] = useState<number | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const requestSequence = useRef(0);
  const inputRef = useRef<HTMLInputElement | null>(null);
  const optionsId = `${useId()}-directory-options`;
  const visible = useMemo(() => listing?.entries.filter((entry) => entry.name.toLowerCase().startsWith(pathText.slice(directoryInputValue(listing.path).length).toLowerCase())) ?? [], [listing, pathText]);
  const interactive = (entry: DirectoryEntry) => entry.isDirectory || mode === 'file';
  const load = (path?: string) => {
    if (path !== undefined && !path.startsWith('/')) { setError('Enter an absolute path'); return; }
    const sequence = ++requestSequence.current; setError(null); setLoading(true);
    void listDirectory(path).then((next) => {
      if (requestSequence.current !== sequence) return;
      setListing(next);
      setPathText(directoryInputValue(next.path));
      const firstInteractive = next.entries.findIndex(interactive);
      setActiveIndex(firstInteractive === -1 ? null : firstInteractive);
      setLoading(false);
      requestAnimationFrame(() => requestAnimationFrame(() => inputRef.current?.focus()));
    }).catch((reason: unknown) => {
      if (requestSequence.current === sequence) { setLoading(false); setError(reason instanceof Error ? reason.message : 'Failed to list directory'); }
    });
  };
  // eslint-disable-next-line react-hooks/exhaustive-deps -- initialPath is a mount-time seed; navigation owns all later loads.
  useEffect(() => { load(initialPath ?? undefined); }, []);
  const move = (delta: 1 | -1) => {
    if (visible.length === 0) return;
    let index = activeIndex ?? (delta === 1 ? -1 : visible.length);
    do { index += delta; } while (index >= 0 && index < visible.length && !interactive(visible[index]));
    if (index >= 0 && index < visible.length) setActiveIndex(index);
  };
  const onKeyDown = (event: KeyboardEvent<HTMLInputElement>) => {
    const entry = activeIndex === null ? undefined : visible[activeIndex];
    if (event.key === 'ArrowDown' || event.key === 'ArrowUp') { event.preventDefault(); move(event.key === 'ArrowDown' ? 1 : -1); }
    else if (event.key === 'Escape') { event.preventDefault(); onCancel(); }
    else if (event.key === '/' && entry?.isDirectory) { event.preventDefault(); load(entry.path); }
    else if (event.key === 'Enter') {
      event.preventDefault();
      if (!pathText.startsWith('/')) { setError('Enter an absolute path'); return; }
      if (entry?.isDirectory) load(entry.path);
      else if (entry && mode === 'file') onSelect(entry.path);
      else if (listing && pathText === directoryInputValue(listing.path)) onSelect(listing.path);
      else load(normalizeDirectoryPath(pathText));
    }
  };
  const matchesListing = listing !== null && pathText === directoryInputValue(listing.path);
  const parent = listing?.parent ?? null;
  const empty = listing !== null && listing.entries.length === 0;
  return (
    <section className={styles.browser}>
      {/* A grid and not astryx's `HStack`: the field has to take the rest of
          the row, and `TextInput` sizes itself through `width` on its own
          `Field` wrapper — `1fr` is the only place that width can come from
          without hard-coding one. */}
      <div className={styles.head}>
        <Button
          type="button"
          variant="secondary"
          isIconOnly
          icon={<Icon name="arrow-up" />}
          label="Parent directory"
          isDisabled={parent === null || loading}
          onClick={() => { if (parent !== null) load(parent); }}
        />
        <TextInput
          ref={inputRef}
          label="Directory path"
          isLabelHidden
          className={styles.path}
          width="100%"
          role="combobox"
          aria-controls={optionsId}
          aria-expanded
          aria-activedescendant={activeIndex === null ? undefined : `${optionsId}-option-${activeIndex}`}
          value={pathText}
          placeholder="/absolute/path"
          onChange={(next) => { setPathText(next); setActiveIndex(null); }}
          onKeyDown={onKeyDown}
        />
      </div>

      {/* The list keeps rendering the entries it already has while the next
          listing loads, so this is a status *beside* it rather than a state
          that replaces it — a reload must not blank the rows under the
          pointer.

          Text and no spinner: astryx's `Spinner` paints itself on a `<canvas>`
          through `useTheme`, so it needs `matchMedia` and a 2D context that the
          jsdom tier has neither of, and a control this small does not earn a
          browser-tier test of its own to buy one. */}
      {loading && <p className={styles.status} role="status">Loading…</p>}
      {error !== null && <Banner status="error" title={error} />}

      <ul id={optionsId} className={styles.list} role="listbox" aria-label="Directory entries">
        {visible.map((entry, index) => (
          <li key={entry.path} role="none">
            <button
              id={`${optionsId}-option-${index}`}
              className={styles.entry}
              role="option"
              type="button"
              aria-selected={index === activeIndex}
              aria-disabled={!interactive(entry) || undefined}
              onMouseMove={() => { if (interactive(entry)) setActiveIndex(index); }}
              onClick={() => { if (entry.isDirectory) load(entry.path); else if (mode === 'file') onSelect(entry.path); }}
            >
              <span className={styles.entryIcon} data-nc-role="icon">
                <Icon name={entry.isDirectory ? 'folder' : 'file'} size="sm" />
              </span>
              <span className={styles.entryName}>{entry.name}</span>
            </button>
          </li>
        ))}
        {/* `role="none"` for both: a listbox owns options, and neither of these
            rows is one. They are two different facts — "this directory holds
            nothing" and "what you typed matches nothing in it" — and telling
            them apart is the difference between navigating on and backing up a
            character. */}
        {listing !== null && !loading && visible.length === 0 && (
          <li className={styles.placeholder} role="none">
            {empty ? 'Empty directory' : 'No matches'}
          </li>
        )}
      </ul>

      <div className={styles.actions}>
        <Button type="button" variant="ghost" label="Cancel" onClick={onCancel} />
        <Button
          type="button"
          variant="primary"
          label={selectLabel}
          isDisabled={!matchesListing}
          onClick={() => { if (listing) onSelect(listing.path); }}
        />
      </div>
    </section>
  );
}
