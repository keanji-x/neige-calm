import { useEffect, useMemo, useRef, type KeyboardEvent } from 'react';
import { useState } from '../state/public.ts';

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
  const visible = useMemo(() => listing?.entries.filter((entry) => entry.name.toLowerCase().startsWith(pathText.slice(directoryInputValue(listing.path).length).toLowerCase())) ?? [], [listing, pathText]);
  const interactive = (entry: DirectoryEntry) => entry.isDirectory || mode === 'file';
  const load = (path?: string) => {
    if (path !== undefined && !path.startsWith('/')) { setError('Enter an absolute path'); return; }
    const sequence = ++requestSequence.current; setError(null); setLoading(true);
    void listDirectory(path).then((next) => {
      if (requestSequence.current !== sequence) return;
      setListing(next);
      setPathText(directoryInputValue(next.path));
      setActiveIndex(next.entries.findIndex(interactive));
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
  return <section className="directory-browser">
    <label>Directory path<input ref={inputRef} role="combobox" aria-controls="directory-options" aria-expanded="true"
      aria-activedescendant={activeIndex === null ? undefined : `directory-option-${activeIndex}`}
      value={pathText} onChange={(event) => { setPathText(event.currentTarget.value); setActiveIndex(null); }} onKeyDown={onKeyDown}/></label>
    {loading && <p role="status">Loading…</p>}
    {error && <p role="alert">{error}</p>}
    <ul id="directory-options" role="listbox">{visible.map((entry, index) => <li key={entry.path} role="none"><button
      id={`directory-option-${index}`} role="option" type="button" aria-selected={index === activeIndex}
      aria-disabled={!interactive(entry) || undefined} onMouseMove={() => { if (interactive(entry)) setActiveIndex(index); }}
      onClick={() => { if (entry.isDirectory) load(entry.path); else if (mode === 'file') onSelect(entry.path); }}>{entry.name}</button></li>)}</ul>
    <button type="button" onClick={onCancel}>Cancel</button>
    <button type="button" disabled={!matchesListing} onClick={() => { if (listing) onSelect(listing.path); }}>{selectLabel}</button>
  </section>;
}
