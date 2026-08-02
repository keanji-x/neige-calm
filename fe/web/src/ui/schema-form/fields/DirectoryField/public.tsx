import { useEffect, type ReactNode } from 'react';
import { useState } from '../../../state/public.ts';
import { useDialogView } from '../../../dialog/public.tsx';
import { DirectoryBrowser, type DirectoryMode, type ListDirectory } from '../../../directory-browser/public.tsx';

export interface DirectoryFieldProps {
  value: string; onChange: (path: string) => void; listDirectory: ListDirectory;
  id?: string; placeholder?: string; mode?: DirectoryMode;
}

export function DirectoryField({ value, onChange, listDirectory, id, placeholder = 'Choose a directory…', mode = 'directory' }: DirectoryFieldProps): ReactNode {
  const [browsing, setBrowsing] = useState(false);
  const dialog = useDialogView();
  useEffect(() => {
    if (!dialog) return;
    if (!browsing) { dialog.popView(); return; }
    const cancel = () => setBrowsing(false);
    dialog.pushView({ title: mode === 'file' ? 'Choose a file or folder' : 'Choose a directory', onEscape: cancel,
      body: <DirectoryBrowser listDirectory={listDirectory} initialPath={value || null} mode={mode} onCancel={cancel} onSelect={(path) => { onChange(path); setBrowsing(false); }}/>,
    });
    return () => dialog.popView();
  // eslint-disable-next-line react-hooks/exhaustive-deps -- capture value and callbacks only when browsing toggles; value changes must not repush the child view.
  }, [browsing, dialog]);
  return <div className="directory-field"><button type="button" id={id} aria-haspopup="dialog" title={value || placeholder} onClick={() => setBrowsing(true)}>
    <span>{value || placeholder}</span><span>Browse…</span></button>
    {browsing && !dialog && <DirectoryBrowser listDirectory={listDirectory} initialPath={value || null} mode={mode} onCancel={() => setBrowsing(false)} onSelect={(path) => { onChange(path); setBrowsing(false); }}/>}</div>;
}
