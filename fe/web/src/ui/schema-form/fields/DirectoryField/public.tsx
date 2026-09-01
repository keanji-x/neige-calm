// The directory field: the control that stands for a folder, and the browse
// view behind it.
//
// This revision is the visual layer only (#1228) — the props surface, the
// Dialog child-view push and the inline fallback are the frozen §6.7b contract
// and are unchanged. What it replaces is a bare `<button>` holding two bare
// `<span>`s, which rendered as browser defaults ("/home/kenjiBrowse…" run
// together) inside a dialog whose every other control is astryx's.
//
// ## Two states, and only one of them is a row
//
// Unset, the control is the folder mark and nothing else. A path is the only
// thing this field has to say, and when it has no path a full-width box saying
// so is a placeholder pretending to be a value: it takes a form row, it takes
// the eye first among the controls beside it, and it says nothing. Set, the
// mark is joined by the folder's own name — the segment that identifies it —
// and the control grows to exactly that.
//
// The `Browse…` affordance is gone with it. It labelled what the whole control
// already is: a button that opens a picker, marked with a folder, carrying
// `aria-haspopup="dialog"`. Two words to say "clickable" is the kind of thing
// that reads as thorough and lands as noise.
//
// ## Names, since there is not always text to take one from
//
//   * The name is always the full path, or the placeholder while there is
//     none — set through `aria-label`, because the visible text is the
//     basename and a reader who hears only "app" learns nothing about *which*
//     `app`. A call site therefore does not need to wrap this in a labelled
//     field, and should not: an outer `<label for>` would outrank nothing here
//     and only split one name into two.
//   * The native `title` carries the same string for the pointer, so the full
//     path is one hover away from the basename. astryx drops `title` from
//     `BaseProps` in favour of its `tooltip` prop, which would mount a
//     floating layer on a control whose whole job is to open one, so the
//     attribute is passed through as a rest prop instead.

import { useEffect, type ReactNode } from 'react';
import { Button } from '@astryxdesign/core/Button';

import { useState } from '../../../state/public.ts';
import { useDialogView } from '../../../dialog/public.tsx';
import { Icon } from '../../../icon/public.tsx';
import { DirectoryBrowser, type DirectoryMode, type ListDirectory } from '../../../directory-browser/public.tsx';
import styles from './directory-field.module.css';

/**
 * The segment that identifies a path: its last one, or `/` for the root, which
 * has none. An empty value has no basename and renders no text at all.
 */
function basenameOf(path: string): string {
  if (path === '') return '';
  const trimmed = path.replace(/\/+$/, '');
  return trimmed === '' ? '/' : trimmed.slice(trimmed.lastIndexOf('/') + 1);
}

export interface DirectoryFieldProps {
  value: string; onChange: (path: string) => void; listDirectory: ListDirectory;
  id?: string; placeholder?: string; mode?: DirectoryMode;
}

export function DirectoryField({ value, onChange, listDirectory, id, placeholder = 'Choose a directory…', mode = 'directory' }: DirectoryFieldProps): ReactNode {
  const [browsing, setBrowsing] = useState(false);
  const dialog = useDialogView();
  const initialPath = mode === 'file' && value ? value.slice(0, value.lastIndexOf('/')) || '/' : value || null;
  useEffect(() => {
    if (!dialog || !browsing) return;
    const cancel = () => setBrowsing(false);
    return dialog.pushView({ title: mode === 'file' ? 'Choose a file or folder' : 'Choose a directory', onEscape: cancel,
      body: <DirectoryBrowser listDirectory={listDirectory} initialPath={initialPath} mode={mode} onCancel={cancel} onSelect={(path) => { onChange(path); setBrowsing(false); }}/>,
    });
  // eslint-disable-next-line react-hooks/exhaustive-deps -- capture value and callbacks only when browsing toggles; value changes must not repush the child view.
  }, [browsing, dialog]);
  /* The one attribute astryx's prop surface has no room for; see the header. */
  const nativeTitle: { title: string } = { title: value || placeholder };
  return (
    <div className={styles.field}>
      <Button
        type="button"
        id={id}
        variant="secondary"
        size="sm"
        className={styles.trigger}
        aria-haspopup="dialog"
        aria-label={value || placeholder}
        {...nativeTitle}
        data-nc-empty={value === '' || undefined}
        icon={<Icon name="folder" size="sm" />}
        /* The basename, not the path: this control sits in a row of controls,
           and a row cannot hold "/home/kenji/src/neige-calm" without becoming
           the row. The identifying segment is the last one; the whole path is
           in both the name and the title, one hover or one screen reader away.
           Empty while unset — the mark is the whole control there. */
        label={basenameOf(value)}
        onClick={() => setBrowsing(true)}
      />
      {browsing && !dialog && (
        <DirectoryBrowser listDirectory={listDirectory} initialPath={initialPath} mode={mode} onCancel={() => setBrowsing(false)} onSelect={(path) => { onChange(path); setBrowsing(false); }}/>
      )}
    </div>
  );
}
