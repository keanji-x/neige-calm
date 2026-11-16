import { useEffect } from 'react';

import { Dialog, useDialogView, type DialogOrigin } from '../../ui/dialog/public.tsx';
import { DirectoryBrowser, type DirectoryMode, type ListDirectory } from '../../ui/directory-browser/public.tsx';

export interface DirectoryPickerProps {
  open: boolean;
  origin?: DialogOrigin;
  initialPath: string | null;
  listDirectory: ListDirectory;
  mode?: DirectoryMode;
  title?: string;
  onCancel: () => void;
  onSelect: (path: string) => void;
}

/**
 * The Area editor pushes the picker into its dialog. The New Track composer
 * hosts the same picker in its own dialog.
 */
export function DirectoryPicker({
  open,
  origin = { kind: 'surface-center' },
  initialPath,
  listDirectory,
  mode = 'directory',
  title = 'Choose a directory',
  onCancel,
  onSelect,
}: DirectoryPickerProps) {
  const dialog = useDialogView();

  useEffect(() => {
    if (!dialog || !open) return;
    return dialog.pushView({
      title,
      onEscape: onCancel,
      body: (
        <DirectoryBrowser
          listDirectory={listDirectory}
          initialPath={initialPath}
          mode={mode}
          onCancel={onCancel}
          onSelect={onSelect}
        />
      ),
    });
  // eslint-disable-next-line react-hooks/exhaustive-deps -- capture the path and ports once when browsing opens; selection must not push a second view.
  }, [open, dialog]);

  if (dialog !== null) return null;

  return (
    <Dialog
      purpose="form"
      origin={origin}
      open={open}
      onClose={onCancel}
      title={title}
      wide
    >
      <DirectoryBrowser
        listDirectory={listDirectory}
        initialPath={initialPath}
        mode={mode}
        onCancel={onCancel}
        onSelect={onSelect}
      />
    </Dialog>
  );
}
