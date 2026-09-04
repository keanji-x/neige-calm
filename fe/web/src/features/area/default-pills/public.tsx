// The two compact creation preferences shared by the Area editor and the New
// Track composer. Their hosts own state and directory browsing; this module
// owns the visible controls so the two surfaces cannot drift into different
// field chrome or different selection semantics.

import { Button } from '@astryxdesign/core/Button';
import { Divider } from '@astryxdesign/core/Divider';
import { DropdownMenu, DropdownMenuItem } from '@astryxdesign/core/DropdownMenu';
import { HoverCard } from '@astryxdesign/core/HoverCard';
import { Icon as AstryxIcon } from '@astryxdesign/core/Icon';
import { List, ListItem } from '@astryxdesign/core/List';
import { VisuallyHidden } from '@astryxdesign/core/VisuallyHidden';
import { useRef, type KeyboardEvent, type ReactNode } from 'react';

import type { TrackRecipe, TrackTemplate } from '../../../../../core/domain/track.ts';
import { Icon } from '../../../ui/icon/public.tsx';
import { useState } from '../../../ui/state/public.ts';
import styles from './default-pills.module.css';

export const NO_TEMPLATE_ID = '';
const NO_TEMPLATE_LABEL = 'No template';

/** A tagged value keeps the independent template and recipe id spaces apart. */
export type StartingPoint =
  | Readonly<{ kind: 'none' }>
  | Readonly<{ kind: 'template'; id: string }>
  | Readonly<{ kind: 'recipe'; id: string }>;

export const NO_STARTING_POINT: StartingPoint = Object.freeze({ kind: 'none' });

function basenameOf(path: string): string {
  const trimmed = path.replace(/\/+$/, '');
  return trimmed === '' ? '/' : trimmed.slice(trimmed.lastIndexOf('/') + 1);
}

export function TemplatePill({
  templates, templatesLoaded, value, onChange, placement, controlLabel = 'Template', triggerId,
  isDisabled = false,
}: Readonly<{
  templates: readonly TrackTemplate[];
  templatesLoaded: boolean;
  value: string;
  onChange: (value: string) => void;
  placement: 'above' | 'below';
  controlLabel?: string;
  triggerId?: string;
  isDisabled?: boolean;
}>) {
  const selected: StartingPoint = value === NO_TEMPLATE_ID
    ? NO_STARTING_POINT
    : { kind: 'template', id: value };
  return (
    <StartingPointPill
      templates={templates}
      templatesLoaded={templatesLoaded}
      value={selected}
      onChange={(next) => {
        if (next.kind === 'recipe') return;
        onChange(next.kind === 'none' ? NO_TEMPLATE_ID : next.id);
      }}
      placement={placement}
      controlLabel={controlLabel}
      triggerId={triggerId}
      isDisabled={isDisabled}
    />
  );
}

/**
 * The shared compact starting-point control. New Track supplies recipes and
 * its manage action; the Area editor uses the built-in-template-only adapter
 * above. Keeping the controlled menu and its Escape focus handoff here avoids
 * two subtly different pill implementations.
 */
export function StartingPointPill({
  templates, templatesLoaded, recipes = [], value, onChange, placement,
  onManageRecipes, controlLabel = 'Template', triggerId, isDisabled = false,
}: Readonly<{
  templates: readonly TrackTemplate[];
  templatesLoaded: boolean;
  recipes?: readonly TrackRecipe[];
  value: StartingPoint;
  onChange: (value: StartingPoint) => void;
  placement: 'above' | 'below';
  onManageRecipes?: () => void;
  controlLabel?: string;
  triggerId?: string;
  isDisabled?: boolean;
}>) {
  const [open, setOpen] = useState(false);
  const hostRef = useRef<HTMLSpanElement | null>(null);
  const chosen = value.kind === 'template'
    ? templates.find((template) => template.id === value.id)
    : undefined;
  const chosenRecipe = value.kind === 'recipe'
    ? recipes.find((recipe) => recipe.id === value.id)
    : undefined;
  const unresolved = value.kind === 'template' && chosen === undefined;
  const unavailable = unresolved && templatesLoaded;
  const selectedLabel = chosen?.title ?? chosenRecipe?.title
    ?? (value.kind === 'none'
      ? NO_TEMPLATE_LABEL
      : `${value.id}${unavailable ? ' (unavailable)' : ''}`);
  const controlName = `${controlLabel}: ${selectedLabel}`;
  const showGroupHeadings = recipes.length > 0 && templates.length > 0;
  const closeOnEscape = (event: KeyboardEvent<HTMLSpanElement>) => {
    if (event.key !== 'Escape' || !open) return;
    // Own Escape before a host Dialog's document listener sees it. Some menu
    // rows also host a HoverCard whose native Escape listener prevents the
    // DropdownMenu's delegated handler from restoring focus reliably.
    event.preventDefault();
    event.stopPropagation();
    setOpen(false);
    requestAnimationFrame(() => hostRef.current?.querySelector('button')?.focus());
  };
  return (
    <span ref={hostRef} className={styles.host} onKeyDownCapture={closeOnEscape}>
      <DropdownMenu
        placement={placement}
        isMenuOpen={open}
        onOpenChange={setOpen}
        button={{
          id: triggerId,
          label: controlName,
          children: selectedLabel,
          variant: 'secondary',
          size: 'sm',
          isDisabled,
          className: styles.trigger,
        }}
      >
        <TemplateChoice
          label={NO_TEMPLATE_LABEL}
          isSelected={value.kind === 'none'}
          onSelect={() => onChange(NO_STARTING_POINT)}
        />
        {unresolved && (
          <TemplateChoice
            label={selectedLabel}
            isSelected
            isDisabled
            onSelect={() => undefined}
          />
        )}
        <MenuGroup heading={showGroupHeadings ? 'My recipes' : null}>
          {recipes.map((recipe) => (
            <TemplateChoice
              key={`recipe:${recipe.id}`}
              label={recipe.title}
              isSelected={value.kind === 'recipe' && value.id === recipe.id}
              onSelect={() => onChange({ kind: 'recipe', id: recipe.id })}
            />
          ))}
        </MenuGroup>
        <MenuGroup heading={showGroupHeadings ? 'Built in' : null}>
          {templates.map((template) => (
            <TemplateChoice
              key={`template:${template.id}`}
              label={template.title}
              tasks={template.tasks}
              isSelected={value.kind === 'template' && value.id === template.id}
              onSelect={() => onChange({ kind: 'template', id: template.id })}
            />
          ))}
        </MenuGroup>
        {onManageRecipes !== undefined && (
          <>
            <Divider />
            <DropdownMenuItem label="Manage recipes…" onClick={onManageRecipes} />
          </>
        )}
      </DropdownMenu>
    </span>
  );
}

export function FolderPill({
  value, emptyLabel = 'Neige workspace', controlLabel = 'Folder', clearLabel,
  buttonId, onBrowse, onClear, isDisabled = false,
}: Readonly<{
  value: string;
  emptyLabel?: string;
  controlLabel?: string;
  clearLabel: string;
  buttonId?: string;
  isDisabled?: boolean;
  onBrowse: () => void;
  onClear: () => void;
}>) {
  const accessibleName = value === '' ? `${controlLabel}: ${emptyLabel}` : `${controlLabel}: ${value}`;
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
        label={value === '' ? emptyLabel : basenameOf(value)}
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

function MenuGroup({ heading, children }: Readonly<{
  heading: string | null;
  children: ReactNode;
}>) {
  if (heading === null) return <>{children}</>;
  return (
    <div role="group" aria-label={heading}>
      <div className={styles.menuGroupHeading} aria-hidden="true">{heading}</div>
      {children}
    </div>
  );
}

function TemplateChoice({
  label, tasks, isSelected, isDisabled = false, onSelect,
}: Readonly<{
  label: string;
  tasks?: TrackTemplate['tasks'];
  isSelected: boolean;
  isDisabled?: boolean;
  onSelect: () => void;
}>) {
  const item = (
    <DropdownMenuItem
      label={label}
      onClick={onSelect}
      isDisabled={isDisabled}
      endContent={isSelected ? (
        <>
          <AstryxIcon icon="check" size="sm" color="accent" />
          <VisuallyHidden>Selected</VisuallyHidden>
        </>
      ) : undefined}
    />
  );
  if (tasks === undefined || tasks.length === 0) return item;
  return (
    <HoverCard
      placement="end"
      focusTrigger="always"
      content={(
        <span className={styles.taskScroll}>
          <List listStyle="decimal" density="compact">
            {tasks.map((task) => (
              <ListItem key={task.key} label={task.key} description={task.goal} />
            ))}
          </List>
        </span>
      )}
    >
      {item}
    </HoverCard>
  );
}
