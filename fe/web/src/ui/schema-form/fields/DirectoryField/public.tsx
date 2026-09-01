// The directory field: the control that stands for a folder, and the browse
// view behind it.
//
// This revision is the visual layer only (#1228) — the props surface, the
// Dialog child-view push and the inline fallback are the frozen §6.7b contract
// and are unchanged. What it replaces is a bare `<button>` holding two bare
// `<span>`s, which rendered as browser defaults ("/home/kenjiBrowse…" run
// together) inside a dialog whose every other control is astryx's.
//
// ## One chip, two states
//
// Unset, the chip is the folder mark and the call site's placeholder, which is
// written as the question the control answers — "Choose a folder". Set, the
// placeholder gives way to the folder's own name: the last segment of the
// path, which is the part that identifies it, in mono because by then the chip
// is holding data and not a question.
//
// What it is *not* is a full-width box with a placeholder in it. That shape
// takes a form row and the eye that goes with it to say nothing, which is how
// an optional setting ends up looking like the subject of the dialog.
//
// The `Browse…` affordance is gone too. It labelled what the whole control
// already is: a button that opens a picker, marked with a folder, carrying
// `aria-haspopup="dialog"`. Two words to say "clickable" is the kind of thing
// that reads as thorough and lands as noise.
//
// ## Names, since the visible text is never the whole value
//
//   * The name is the purpose phrase (the call site's placeholder, or a
//     mode-aware default), and once there is a value that phrase *and* the
//     full path — set through `aria-label`, because neither half can
//     come from the contents: the visible text is a basename, and a reader who
//     hears only "app" learns neither which `app` nor what the control is for.
//     Dropping the purpose half was the first cut of this and it was wrong;
//     "/srv/app, button" is a control whose job you have to infer from a path.
//     A call site therefore does not need to wrap this in a labelled field,
//     and should not: an outer `<label for>` would outrank the whole thing and
//     replace it with one word.
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

export function DirectoryField({ value, onChange, listDirectory, id, placeholder, mode = 'directory' }: DirectoryFieldProps): ReactNode {
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
  /* What this control is for, in one phrase, and the only string it has that
     is not a path: it is the chip's text while there is no value, the first
     half of the accessible name once there is one, and the hover string in
     both states.

     The default is mode-aware because the name is now built from it: a file
     picker that calls itself "Choose a directory" is a wrong name, not a
     stale placeholder, and it used to be neither because nothing read it out.
     A caller that passes `''` falls back to the same default rather than
     naming the control ": /srv/app" — or nothing at all when it is empty.
     The trailing ellipsis is dropped: it is the "this opens something"
     convention, and "Choose a directory…: /srv/app" is the punctuation of a
     control still waiting for input. */
  const purpose = (placeholder ?? '').replace(/…$/u, '').trim()
    || (mode === 'file' ? 'Choose a file' : 'Choose a directory');
  const name = value === '' ? purpose : `${purpose}: ${value}`;
  /* The one attribute astryx's prop surface has no room for; see the header. */
  const nativeTitle: { title: string } = { title: name };
  return (
    <div className={styles.field}>
      <Button
        type="button"
        id={id}
        variant="secondary"
        size="sm"
        className={styles.trigger}
        aria-haspopup="dialog"
        aria-label={name}
        {...nativeTitle}
        data-nc-empty={value === '' || undefined}
        icon={<Icon name="folder" size="sm" />}
        /* Unset, the placeholder — which a call site writes as the question
           the control answers ("Choose a folder"), because a chip that says
           only what it *is* leaves the reader to guess what tapping it does.
           Set, the basename and not the path: this control sits in a row of
           controls, and a row cannot hold "/home/kenji/src/neige-calm" without
           becoming the row. The identifying segment is the last one; the whole
           path is in both the name and the title, one hover or one screen
           reader away. */
        label={value === '' ? purpose : basenameOf(value)}
        onClick={() => setBrowsing(true)}
      />
      {browsing && !dialog && (
        <DirectoryBrowser listDirectory={listDirectory} initialPath={initialPath} mode={mode} onCancel={() => setBrowsing(false)} onSelect={(path) => { onChange(path); setBrowsing(false); }}/>
      )}
    </div>
  );
}
