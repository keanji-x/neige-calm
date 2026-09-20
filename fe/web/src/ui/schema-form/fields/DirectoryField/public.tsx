// The directory field: a chip that opens the browse view. It must not set `aria-label`: a call site wraps it in a `<label htmlFor>`, which `aria-label` would silently outrank. The full path travels as an `aria-describedby` node and the native `title` (passed as a rest prop since astryx drops it from `BaseProps`).

import { useEffect, useId, type ReactNode } from 'react';
import { Button } from '@astryxdesign/core/Button';

import { useState } from '../../../state/public.ts';
import { useDialogView } from '../../../dialog/public.tsx';
import { Icon } from '../../../icon/public.tsx';
import { DirectoryBrowser, type DirectoryMode, type ListDirectory } from '../../../directory-browser/public.tsx';
import styles from './directory-field.module.css';

/** The last segment, or `/` for the root; an empty value renders no text. */
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
  const pathDescriptionId = `${useId()}-directory-field-path`;
  const initialPath = mode === 'file' && value ? value.slice(0, value.lastIndexOf('/')) || '/' : value || null;
  useEffect(() => {
    if (!dialog || !browsing) return;
    const cancel = () => setBrowsing(false);
    return dialog.pushView({ title: mode === 'file' ? 'Choose a file or folder' : 'Choose a directory', onEscape: cancel,
      body: <DirectoryBrowser listDirectory={listDirectory} initialPath={initialPath} mode={mode} onCancel={cancel} onSelect={(path) => { onChange(path); setBrowsing(false); }}/>,
    });
  // eslint-disable-next-line react-hooks/exhaustive-deps -- capture value and callbacks only when browsing toggles; value changes must not repush the child view.
  }, [browsing, dialog]);
  /* The purpose phrase: the chip's text while empty, the first half of the name once set. Mode-aware default; a trailing ellipsis is dropped since it is the "this opens something" convention. */
  const purpose = (placeholder ?? '').replace(/…$/u, '').trim()
    || (mode === 'file' ? 'Choose a file' : 'Choose a directory');
  const name = value === '' ? purpose : `${purpose}: ${value}`;
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
        aria-describedby={value === '' ? undefined : pathDescriptionId}
        {...nativeTitle}
        data-nc-empty={value === '' || undefined}
        icon={<Icon name="folder" size="sm" />}
        /* The basename, not the path: a row of chips cannot hold a full path without becoming the row. */
        label={value === '' ? purpose : basenameOf(value)}
        onClick={() => setBrowsing(true)}
      />
      {/* Rendered only when there is a path: an empty description is a node screen readers still walk into. */}
      {value !== '' && (
        <span className={styles.srOnly} id={pathDescriptionId}>{value}</span>
      )}
      {browsing && !dialog && (
        <DirectoryBrowser listDirectory={listDirectory} initialPath={initialPath} mode={mode} onCancel={() => setBrowsing(false)} onSelect={(path) => { onChange(path); setBrowsing(false); }}/>
      )}
    </div>
  );
}
