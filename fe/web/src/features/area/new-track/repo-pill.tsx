import { useRef } from 'react';
import { flushSync } from 'react-dom';
import { Divider } from '@astryxdesign/core/Divider';
import { DropdownMenu, DropdownMenuItem } from '@astryxdesign/core/DropdownMenu';
import { Icon as AstryxIcon } from '@astryxdesign/core/Icon';
import { Text } from '@astryxdesign/core/Text';
import { VisuallyHidden } from '@astryxdesign/core/VisuallyHidden';

import type { NewTrackBody } from '../../../../../core/domain/track.ts';
import { Icon } from '../../../ui/icon/public.tsx';
import { useState } from '../../../ui/state/public.ts';
import { FolderClearButton, folderLabelParts } from '../default-pills/public.tsx';
import styles from './repo-pill.module.css';

export type RepoIsolationMode = NonNullable<NewTrackBody['isolation']>;

const REPO_MODES = Object.freeze([
  Object.freeze({
    value: 'worktree' as const,
    label: 'Worktree 模式',
    description: '默认 · 安全隔离，创建独立 worktree',
  }),
  Object.freeze({
    value: 'none' as const,
    label: '裸文件夹模式',
    description: '对话使用源文件夹 · 任务 worker 需要 worktree',
  }),
] as const);

export type RepoPillProps = Readonly<{
  buttonId?: string;
  value: string;
  isolation: RepoIsolationMode;
  clearLabel: string;
  isDisabled?: boolean;
  onIsolationChange: (mode: RepoIsolationMode) => void;
  onBrowse: () => void;
  onClear: () => void;
}>;

export function RepoPill({
  buttonId,
  value,
  isolation,
  clearLabel,
  isDisabled = false,
  onIsolationChange,
  onBrowse,
  onClear,
}: RepoPillProps) {
  const [open, setOpen] = useState(false);
  const triggerRef = useRef<HTMLButtonElement | null>(null);
  const { visibleLabel, accessibleName } = folderLabelParts({
    value,
    controlLabel: 'Repo',
    showLabel: true,
    modeSuffix: isolation === 'none' ? '源文件夹' : undefined,
  });

  return (
    <>
      <DropdownMenu
        placement="above"
        isMenuOpen={open && !isDisabled}
        onOpenChange={(next) => setOpen(next && !isDisabled)}
        hasChevron={false}
        button={{
          id: buttonId,
          ref: triggerRef,
          label: accessibleName,
          children: (
            <Text type="inherit" color="inherit" maxLines={1} hasTruncateTooltip>
              {visibleLabel}
            </Text>
          ),
          variant: 'secondary',
          size: 'sm',
          icon: <Icon name="folder" size="sm" />,
          isDisabled,
          className: styles.trigger,
        }}
      >
        <div role="group" aria-label="工作模式">
          <div className={styles.groupHeading} aria-hidden="true">工作模式</div>
          {REPO_MODES.map((option) => {
            const managedOnly = value === '';
            const selected = managedOnly ? option.value === 'worktree' : isolation === option.value;
            const disabled = managedOnly && option.value === 'none';
            return (
              <DropdownMenuItem
                key={option.value}
                label={option.label}
                description={disabled ? '请先通过 Others… 选择外部文件夹' : option.description}
                isDisabled={disabled}
                onClick={() => {
                  if (!disabled) onIsolationChange(option.value);
                }}
                endContent={selected ? (
                  <>
                    <AstryxIcon icon="check" size="sm" color="accent" />
                    <VisuallyHidden>Selected</VisuallyHidden>
                  </>
                ) : undefined}
              />
            );
          })}
        </div>
        <Divider />
        <DropdownMenuItem
          label="Others…"
          hasCloseOnSelect={false}
          description={value !== '' ? value : '浏览并选择文件夹'}
          onClick={() => {
            flushSync(() => setOpen(false));
            // Let the menu's selection event finish before handing focus to the dialog.
            queueMicrotask(() => {
              const trigger = triggerRef.current;
              if (trigger === null) return;
              trigger.focus();
              onBrowse();
            });
          }}
        />
        <DropdownMenuItem
          label="Neige workspace"
          description={value === '' ? '当前使用托管默认工作区' : '切回托管默认工作区'}
          onClick={() => {
            if (value !== '') onClear();
          }}
          endContent={value === '' ? (
            <>
              <AstryxIcon icon="check" size="sm" color="accent" />
              <VisuallyHidden>Selected</VisuallyHidden>
            </>
          ) : undefined}
        />
      </DropdownMenu>
      {value !== '' && <FolderClearButton label={clearLabel} isDisabled={isDisabled} onClear={onClear} />}
    </>
  );
}
