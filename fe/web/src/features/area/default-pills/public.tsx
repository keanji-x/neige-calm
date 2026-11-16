// Shared directory control; callers own the path and directory browsing.
import { Button } from '@astryxdesign/core/Button';
import { Icon as AstryxIcon } from '@astryxdesign/core/Icon';
import { Icon } from '../../../ui/icon/public.tsx';
import styles from './default-pills.module.css';

function basenameOf(path: string): string {
  const trimmed = path.replace(/\/+$/, '');
  return trimmed === '' ? '/' : trimmed.slice(trimmed.lastIndexOf('/') + 1);
}

export function folderLabelParts({
  value,
  emptyLabel = 'Neige workspace',
  controlLabel = 'Folder',
  modeSuffix,
  showLabel = false,
}: Readonly<{
  value: string;
  showLabel?: boolean;
  emptyLabel?: string;
  controlLabel?: string;
  modeSuffix?: string;
}>): Readonly<{ visibleLabel: string; accessibleName: string }> {
  const prefix = showLabel ? `${controlLabel}: ` : '';
  const suffix = value !== '' && modeSuffix !== undefined && modeSuffix !== '' ? ` · ${modeSuffix}` : '';
  const visibleLabel = `${prefix}${value === '' ? emptyLabel : `${basenameOf(value)}${suffix}`}`;
  const accessibleName = value === '' ? `${controlLabel}: ${emptyLabel}`
    : showLabel ? `${visibleLabel} (${value})` : `${controlLabel}: ${value}${suffix}`;
  return { visibleLabel, accessibleName };
}

export function FolderClearButton({
  label,
  isDisabled = false,
  onClear,
}: Readonly<{
  label: string;
  isDisabled?: boolean;
  onClear: () => void;
}>) {
  return (
    <Button
      type="button"
      variant="ghost"
      size="sm"
      isIconOnly
      icon={<AstryxIcon icon="close" size="sm" />}
      label={label}
      isDisabled={isDisabled}
      onClick={onClear}
    />
  );
}

export function FolderPill({
  value, emptyLabel = 'Neige workspace', controlLabel = 'Folder', modeSuffix, clearLabel, showLabel = false,
  buttonId, onBrowse, onClear, isDisabled = false,
}: Readonly<{
  value: string;
  showLabel?: boolean;
  emptyLabel?: string;
  controlLabel?: string;
  modeSuffix?: string;
  clearLabel: string;
  buttonId?: string;
  isDisabled?: boolean;
  onBrowse: () => void;
  onClear: () => void;
}>) {
  const { visibleLabel, accessibleName } = folderLabelParts({
    value, emptyLabel, controlLabel, modeSuffix, showLabel,
  });
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
      {value !== '' && <FolderClearButton label={clearLabel} isDisabled={isDisabled} onClear={onClear} />}
    </>
  );
}
