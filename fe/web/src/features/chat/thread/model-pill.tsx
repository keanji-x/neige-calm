// The model picker in a planner conversation's composer footer. Presentational:
// every value is a prop; the queries and the write live in `app/router`.

import { DropdownMenu, DropdownMenuItem } from '@astryxdesign/core/DropdownMenu';
import { Divider } from '@astryxdesign/core/Divider';
import { HStack } from '@astryxdesign/core/HStack';
import { Icon as AstryxIcon } from '@astryxdesign/core/Icon';
import { Text } from '@astryxdesign/core/Text';
import { VisuallyHidden } from '@astryxdesign/core/VisuallyHidden';
import { useRef, type KeyboardEvent } from 'react';

import type { ModelCatalog, ModelSelection } from '../../../../../core/domain/conversation.ts';
import { useState } from '../../../ui/state/public.ts';
import styles from './model-pill.module.css';

const FOLLOW_DEFAULT_LABEL = 'Default';

/** A standing note rather than a per-switch warning: the catalog carries no context window, so whether a switch compacts is unknowable here. */
const SWITCH_NOTE = 'Switching to a model with a smaller context window can make codex compact the history first.';

export function ModelPill({
  catalog, selection, onChange, isDisabled = false, placement = 'above', triggerId, effortControl = 'separate',
}: Readonly<{
  /** What can be chosen, and what the installation follows. `null` while it loads. */
  catalog: ModelCatalog | null;
  /** What this conversation has chosen. `null` in both fields is "follow the default". */
  selection: ModelSelection;
  onChange: (next: ModelSelection) => void;
  isDisabled?: boolean;
  placement?: 'above' | 'below';
  triggerId?: string;
  /** A narrow host can keep model and effort inside a single menu. */
  effortControl?: 'separate' | 'in-menu';
}>) {
  const [open, setOpen] = useState(false);
  const hostRef = useRef<HTMLSpanElement | null>(null);

  const models = catalog?.models ?? [];
  const unreachable = catalog !== null && catalog.source === 'unavailable';
  const defaultName = catalog?.default_source === 'config_read' || catalog?.default_source === 'config_toml'
    ? catalog.default.model
    : null;
  /* Following the installation default is still running a model, so the effort control belongs here too: these are the efforts of whatever the default resolves to now. */
  const followed = catalog?.default.model == null
    ? undefined
    : models.find((model) => model.model === catalog.default.model);
  const chosen = selection.model === null
    ? followed
    : models.find((model) => model.model === selection.model);
  /* The trigger names the model, not the route to it; `Default` alone only when nothing truer can be said, and an unlisted slug still names what runs. */
  const label = selection.model === null
    ? (defaultName ?? FOLLOW_DEFAULT_LABEL)
    : (chosen?.display_name ?? selection.model);
  /* The accessible name keeps what the visible one dropped. A person reading
     the pill has the menu one press away; a person hearing it does not. */
  const spokenLabel = selection.model === null && defaultName !== null
    ? `Model: ${label} (this installation's default)`
    : `Model: ${label}`;

  const efforts = chosen?.supported_reasoning_efforts ?? [];
  const closeOnEscape = (event: KeyboardEvent<HTMLSpanElement>) => {
    if (event.key !== 'Escape' || !open) return;
    // A host Dialog's document listener would otherwise take Escape first and the trigger would not get its focus back.
    event.preventDefault();
    event.stopPropagation();
    setOpen(false);
    requestAnimationFrame(() => hostRef.current?.querySelector('button')?.focus());
  };

  return (
    <HStack gap={1} align="center" className={styles.group}>
      <span ref={hostRef} className={styles.host} onKeyDownCapture={closeOnEscape}>
        <DropdownMenu
          placement={placement}
          isMenuOpen={open}
          onOpenChange={setOpen}
          hasChevron={false}
          button={{
            id: triggerId,
            label: spokenLabel,
            /* `type`/`color` inherit, and that is load-bearing: `Text` would re-assert body size and primary colour over the trigger's own. */
            children: (
              <Text type="inherit" color="inherit" maxLines={1} hasTruncateTooltip>
                {label}
              </Text>
            ),
            variant: effortControl === 'in-menu' ? 'secondary' : 'ghost',
            size: 'sm',
            isDisabled: isDisabled || unreachable,
            className: styles.trigger,
          }}
        >
          <Choice
            label={defaultName === null ? FOLLOW_DEFAULT_LABEL : `${FOLLOW_DEFAULT_LABEL} (${defaultName})`}
            isSelected={selection.model === null}
            onSelect={() => onChange({ model: null, reasoning_effort: null })}
          />
          {models.map((model) => (
            <Choice
              key={model.id}
              label={model.display_name}
              isSelected={selection.model === model.model}
              /* Switching model drops the effort: one chosen for the previous model may not exist on this one. */
              onSelect={() => onChange({ model: model.model, reasoning_effort: null })}
            />
          ))}
          {models.length === 0 && (
            <DropdownMenuItem
              label={unreachable ? 'codex is not running' : 'No models available on this account'}
              isDisabled
            />
          )}
          {effortControl === 'in-menu' && efforts.length > 1 && (
            <>
              <Divider />
              <div role="group" aria-label="Reasoning effort">
                <div className={styles.groupHeading} aria-hidden="true">Reasoning effort</div>
                <EffortChoices defaultName={selection.model === null ? catalog?.default.reasoning_effort ?? null : chosen?.default_reasoning_effort ?? null} efforts={efforts} value={selection.reasoning_effort}
                  onChange={(effort) => onChange({ model: selection.model, reasoning_effort: effort })} />
              </div>
            </>
          )}
          <Divider />
          <div className={styles.note} role="note">
            <Text type="supporting">{SWITCH_NOTE}</Text>
          </div>
        </DropdownMenu>
      </span>
      {effortControl === 'separate' && efforts.length > 1 && (
        <EffortPill
          efforts={efforts}
          value={selection.reasoning_effort}
          /* While a model is chosen the entry's own `default_reasoning_effort` applies; while the default is followed, the catalog's. */
          defaultName={selection.model === null
            ? catalog?.default.reasoning_effort ?? null
            : chosen?.default_reasoning_effort ?? null}
          isDisabled={isDisabled}
          placement={placement}
          onChange={(effort) => onChange({ model: selection.model, reasoning_effort: effort })}
        />
      )}
    </HStack>
  );
}

function EffortPill({
  efforts, value, defaultName, onChange, placement, isDisabled,
}: Readonly<{
  efforts: ModelCatalog['models'][number]['supported_reasoning_efforts'];
  value: string | null;
  /** What "Default" resolves to, or `null` when nothing has said. */
  defaultName: string | null;
  onChange: (value: string | null) => void;
  placement: 'above' | 'below';
  isDisabled: boolean;
}>) {
  const [open, setOpen] = useState(false);
  const hostRef = useRef<HTMLSpanElement | null>(null);
  /* Same rule as the model trigger: the effort, not the route to it. */
  const label = value ?? defaultName ?? FOLLOW_DEFAULT_LABEL;
  const spokenLabel = value === null && defaultName !== null
    ? `Reasoning effort: ${label} (the default)`
    : `Reasoning effort: ${label}`;
  const closeOnEscape = (event: KeyboardEvent<HTMLSpanElement>) => {
    if (event.key !== 'Escape' || !open) return;
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
        hasChevron={false}
        button={{
          label: spokenLabel,
          children: label,
          variant: 'ghost',
          size: 'sm',
          isDisabled,
          className: styles.effort,
        }}
      >
        <EffortChoices defaultName={defaultName} efforts={efforts} value={value} onChange={onChange} />
      </DropdownMenu>
    </span>
  );
}

function EffortChoices({ efforts, value, defaultName, onChange }: Readonly<{
  defaultName: string | null;
  efforts: ModelCatalog['models'][number]['supported_reasoning_efforts'];
  value: string | null;
  onChange: (value: string | null) => void;
}>) {
  return <>
    <Choice label={defaultName === null ? FOLLOW_DEFAULT_LABEL : `${FOLLOW_DEFAULT_LABEL} (${defaultName})`} isSelected={value === null}
      onSelect={() => onChange(null)} />
    {efforts.map((effort) => (
      <Choice key={effort.reasoning_effort} label={effort.reasoning_effort}
        description={effort.description} isSelected={value === effort.reasoning_effort}
        onSelect={() => onChange(effort.reasoning_effort)} />
    ))}
  </>;
}

function Choice({
  label, description, isSelected, isDisabled = false, onSelect,
}: Readonly<{
  label: string;
  description?: string;
  isSelected: boolean;
  isDisabled?: boolean;
  onSelect: () => void;
}>) {
  return (
    <DropdownMenuItem
      label={label}
      description={description}
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
}
