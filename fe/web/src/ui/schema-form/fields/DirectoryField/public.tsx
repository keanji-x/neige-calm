// The directory field: the collapsed row that stands where a form control
// stands, and the browse view behind it.
//
// This revision is the visual layer only (#1228) — the props surface, the
// Dialog child-view push and the inline fallback are the frozen §6.7b contract
// and are unchanged. What it replaces is a bare `<button>` holding two bare
// `<span>`s, which rendered as browser defaults ("/home/kenjiBrowse…" run
// together) inside a dialog whose every other control is astryx's.
//
// The trigger is an astryx `Button` for the same reason the form around it is:
// it is a plain control and astryx owns those. Three details are ours:
//
//   * The value is the path, so it is mono, and it truncates — a field this
//     wide cannot show a deep path. astryx's own label span already carries
//     `overflow: hidden` + `text-overflow: ellipsis` + `min-width: 0`, so the
//     module only has to make the row full-width for that to take effect.
//   * The native `title` survives. It is part of the frozen contract — a
//     truncated path has to be readable without opening the browser — but
//     astryx drops `title` from `BaseProps` in favour of its `tooltip` prop,
//     which would mount a floating layer on a control whose whole job is to
//     open one. `Button` spreads its rest props onto the `<button>`, so the
//     attribute is passed through as one, and the type-level omission is
//     stepped around in exactly this one place rather than accepted as a
//     behaviour change.
//   * The placeholder is dimmed rather than absent. This field is optional at
//     its only call site, and an empty control that looks broken is how an
//     optional field gets filled in by accident.
//
// `label` + `endContent`, and deliberately not `children`: astryx sets
// `aria-label={label}` as soon as `children` differ from it (`Button.tsx`),
// and this button's name is owned by the `<label for>` its call site puts on
// it — an `aria-label` here would silently outrank that and rename the control
// from "Folder" to whatever path it currently holds.

import { useEffect, type ReactNode } from 'react';
import { Button } from '@astryxdesign/core/Button';

import { useState } from '../../../state/public.ts';
import { useDialogView } from '../../../dialog/public.tsx';
import { Icon } from '../../../icon/public.tsx';
import { DirectoryBrowser, type DirectoryMode, type ListDirectory } from '../../../directory-browser/public.tsx';
import styles from './directory-field.module.css';

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
        className={styles.trigger}
        aria-haspopup="dialog"
        {...nativeTitle}
        data-nc-empty={value === '' || undefined}
        icon={<Icon name="folder" size="sm" />}
        label={value || placeholder}
        endContent={<span className={styles.browse}>Browse…</span>}
        onClick={() => setBrowsing(true)}
      />
      {browsing && !dialog && (
        <DirectoryBrowser listDirectory={listDirectory} initialPath={initialPath} mode={mode} onCancel={() => setBrowsing(false)} onSelect={(path) => { onChange(path); setBrowsing(false); }}/>
      )}
    </div>
  );
}
