// Shared directory control; callers own the path and directory browsing.
import { Button } from '@astryxdesign/core/Button';
import { Icon as AstryxIcon } from '@astryxdesign/core/Icon';
import { Icon } from '../../../ui/icon/public.tsx';
import styles from './default-pills.module.css';

function basenameOf(path: string): string {
  const trimmed = path.replace(/\/+$/, '');
  return trimmed === '' ? '/' : trimmed.slice(trimmed.lastIndexOf('/') + 1);
}

export function FolderPill({
  value, emptyLabel = 'Neige workspace', controlLabel = 'Folder', clearLabel, showLabel = false,
  buttonId, onBrowse, onClear, isDisabled = false,
}: Readonly<{
  value: string;
  showLabel?: boolean;
  emptyLabel?: string;
  controlLabel?: string;
  clearLabel: string;
  buttonId?: string;
  isDisabled?: boolean;
  onBrowse: () => void;
  onClear: () => void;
}>) {
  const visibleLabel = `${showLabel ? `${controlLabel}: ` : ''}${value === '' ? emptyLabel : basenameOf(value)}`;
  const accessibleName = value === '' ? `${controlLabel}: ${emptyLabel}`
    : showLabel ? `${visibleLabel} (${value})` : `${controlLabel}: ${value}`;
  return (
    <>
      <Button
        type="button"
        id={buttonId}
        variant="secondary"
        size="sm"
        isDisabled={isDisabled}
        className={styles.trigger}
        aria-haspopup="dialog"
        aria-label={accessibleName}
        icon={<Icon name="folder" size="sm" />}
        label={visibleLabel}
        onClick={onBrowse}
        {...{ title: accessibleName }}
      />
      {value !== '' && (
        <Button
          type="button"
          variant="ghost"
          size="sm"
          isIconOnly
          icon={<AstryxIcon icon="close" size="sm" />}
          label={clearLabel}
          isDisabled={isDisabled}
          onClick={onClear}
        />
      )}
    </>
  );
}
