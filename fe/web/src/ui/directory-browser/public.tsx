// The directory browser: an editable absolute path that is also a combobox over the listing it names. The listbox stays local markup: astryx's `List`/`Item` hard-code roles and cannot host an `aria-activedescendant` list driven from a focused input.

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
  /* Arrow keys move `aria-activedescendant`, not DOM focus, so nothing scrolls the bounded list for us; `block: 'nearest'` keeps hover from yanking it, and the `typeof` guard is for jsdom. */
  useEffect(() => {
    if (activeIndex === null) return;
    const option = document.getElementById(`${optionsId}-option-${activeIndex}`);
    if (typeof option?.scrollIntoView !== 'function') return;
    option.scrollIntoView({ block: 'nearest' });
  }, [activeIndex, optionsId]);
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
      {/* A grid and not `HStack`: `TextInput` sizes through its own `Field` wrapper, so `1fr` is the only place its width can come from. */}
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

      {/* A status beside the list, not replacing it: a reload must not blank the rows under the pointer. No spinner — astryx's paints on a canvas jsdom lacks. */}
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
        {/* `role="none"`: a listbox owns options, and neither placeholder row is one. */}
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
