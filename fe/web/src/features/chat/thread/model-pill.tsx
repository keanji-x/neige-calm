// The model picker in a planner conversation's composer footer, and on the new-track page. Presentational:
// every value is a prop; the queries and the write live in `app/router`. Each provider's catalog is one
// group; on the new-track page the pick also decides the Planner's provider (#1810), and each group
// follows its provider's availability (#1817).

import { DropdownMenu, DropdownMenuItem } from '@astryxdesign/core/DropdownMenu';
import { Divider } from '@astryxdesign/core/Divider';
import { HStack } from '@astryxdesign/core/HStack';
import { Icon as AstryxIcon } from '@astryxdesign/core/Icon';
import { Text } from '@astryxdesign/core/Text';
import { VisuallyHidden } from '@astryxdesign/core/VisuallyHidden';
import { Fragment, useRef, type KeyboardEvent } from 'react';

import type { AgentProvider } from '../../../../../core/api/generated/wire.ts';
import type { ProviderAvailability } from '../../../../../core/domain/agent-providers.ts';
import type { ModelCatalog, ModelSelection } from '../../../../../core/domain/conversation.ts';
import { useState } from '../../../ui/state/public.ts';
import styles from './model-pill.module.css';

const FOLLOW_DEFAULT_LABEL = 'Default';

/** A standing note rather than a per-switch warning: the catalog carries no context window, so whether a switch compacts is unknowable here. */
const SWITCH_NOTE = 'Switching to a model with a smaller context window can make codex compact the history first.';

/**
 * Total over `AgentProvider`, so a new backend is a compile error here rather than a missing group.
 * `unavailable` says why that provider's `source: 'unavailable'` catalog is empty. A Claude group waits for
 * its availability and catalog before it joins a menu that offers other providers (`hiddenUntilKnown`); a
 * Codex one stays while the daemon is down, saying so.
 * A Claude Planner has no reasoning-effort choice (`effort: false`), and only codex compacts (`switchNote`).
 */
const PROVIDERS: Readonly<Record<AgentProvider, Readonly<{
  label: string; unavailable: string; hiddenUntilKnown: boolean; effort: boolean; switchNote: boolean;
}>>> = Object.freeze({
  codex: Object.freeze({
    label: 'Codex', unavailable: 'codex is not running', hiddenUntilKnown: false, effort: true, switchNote: true,
  }),
  claude: Object.freeze({
    label: 'Claude', unavailable: 'This server does not run Claude Planners', hiddenUntilKnown: true,
    effort: false, switchNote: false,
  }),
});

/**
 * One provider's section of the menu: its catalog, `null` while it loads, and whether the provider can run
 * right now (`GET /api/agent-providers`). `availability` is `null` while that answer is unknown, and for a
 * conversation that already exists: its turns keep issue-time handling (#1817).
 */
export type ModelGroup = Readonly<{
  provider: AgentProvider;
  catalog: ModelCatalog | null;
  availability: ProviderAvailability | null;
}>;

type CatalogEntry = ModelCatalog['models'][number];

/**
 * The groups a menu shows. The selection's own group always shows, so a pick never passes for another
 * provider's. Any other group is hidden while its provider is `not_configured` on this server.
 */
function visibleModelGroups(groups: readonly ModelGroup[], provider: AgentProvider): readonly ModelGroup[] {
  return groups.filter((group) => group.provider === provider || (
    group.availability?.status !== 'not_configured'
    && (!PROVIDERS[group.provider].hiddenUntilKnown || (group.availability !== null && group.catalog !== null))));
}

/** The name of the default a catalog says is followed, or `null` when it cannot say. */
function defaultNameOf(catalog: ModelCatalog | null): string | null {
  return catalog?.default_source === 'config_read' || catalog?.default_source === 'config_toml'
    ? catalog.default.model
    : null;
}

export function ModelPill({
  groups, provider, selection, onChange, isDisabled = false, placement = 'above', triggerId, effortControl = 'separate',
}: Readonly<{
  /** What can be chosen, one group per provider, in menu order. A conversation passes only its own provider's. */
  groups: readonly ModelGroup[];
  /** The provider `selection` belongs to. */
  provider: AgentProvider;
  /** What this conversation has chosen. `null` in both fields is "follow the default". */
  selection: ModelSelection;
  /** A pick in another group also hands back that group's provider. */
  onChange: (next: ModelSelection, provider: AgentProvider) => void;
  isDisabled?: boolean;
  placement?: 'above' | 'below';
  triggerId?: string;
  /** A narrow host can keep model and effort inside a single menu. */
  effortControl?: 'separate' | 'in-menu';
}>) {
  const [open, setOpen] = useState(false);
  const hostRef = useRef<HTMLSpanElement | null>(null);

  const shown = visibleModelGroups(groups, provider);
  const grouped = shown.length > 1;
  const catalog = groups.find((group) => group.provider === provider)?.catalog ?? null;
  const models = catalog?.models ?? [];
  const unreachable = shown.length > 0
    && shown.every((group) => group.catalog !== null && group.catalog.source === 'unavailable');
  const defaultName = defaultNameOf(catalog);
  /* Following the installation default is still running a model, so the effort control belongs here too: these are the efforts of whatever the default resolves to now. */
  const followed = catalog?.default.model == null
    ? undefined
    : models.find((model) => model.model === catalog.default.model);
  const chosen = selection.model === null
    ? followed
    : models.find((model) => model.model === selection.model);
  /* The trigger names the model, not the route to it; `Default` alone only when nothing truer can be said, and an unlisted slug still names what runs. */
  const named = selection.model === null
    ? (defaultName ?? FOLLOW_DEFAULT_LABEL)
    : (chosen?.display_name ?? selection.model);
  /* With more than one provider on offer, the pick is a provider too, and the trigger says whose. */
  const label = grouped ? `${PROVIDERS[provider].label} ${named}` : named;
  /* The accessible name keeps what the visible one dropped. A person reading
     the pill has the menu one press away; a person hearing it does not. */
  const spokenLabel = selection.model === null && defaultName !== null
    ? `Model: ${label} (this installation's default)`
    : `Model: ${label}`;

  const efforts = PROVIDERS[provider].effort ? chosen?.supported_reasoning_efforts ?? [] : [];
  const effortDefault = selection.model === null
    ? catalog?.default.reasoning_effort ?? null
    : chosen?.default_reasoning_effort ?? null;
  const switchNote = shown.some((group) => PROVIDERS[group.provider].switchNote);
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
          {shown.map((group, index) => {
            const choices = (
              <GroupChoices group={group} selection={group.provider === provider ? selection : null}
                onChange={(next) => onChange(next, group.provider)} />
            );
            return grouped ? (
              <Fragment key={group.provider}>
                {index > 0 && <Divider />}
                <div role="group" aria-label={PROVIDERS[group.provider].label}>
                  <div className={styles.groupHeading} aria-hidden="true">{PROVIDERS[group.provider].label}</div>
                  {choices}
                </div>
              </Fragment>
            ) : <Fragment key={group.provider}>{choices}</Fragment>;
          })}
          {effortControl === 'in-menu' && efforts.length > 1 && (
            <>
              <Divider />
              <div role="group" aria-label="Reasoning effort">
                <div className={styles.groupHeading} aria-hidden="true">Reasoning effort</div>
                <EffortChoices defaultName={effortDefault} efforts={efforts} value={selection.reasoning_effort}
                  onChange={(effort) => onChange({ model: selection.model, reasoning_effort: effort }, provider)} />
              </div>
            </>
          )}
          {switchNote && (
            <>
              <Divider />
              <div className={styles.note} role="note">
                <Text type="supporting">{SWITCH_NOTE}</Text>
              </div>
            </>
          )}
        </DropdownMenu>
      </span>
      {effortControl === 'separate' && efforts.length > 1 && (
        <EffortPill
          efforts={efforts}
          value={selection.reasoning_effort}
          /* While a model is chosen the entry's own `default_reasoning_effort` applies; while the default is followed, the catalog's. */
          defaultName={effortDefault}
          isDisabled={isDisabled}
          placement={placement}
          onChange={(effort) => onChange({ model: selection.model, reasoning_effort: effort }, provider)}
        />
      )}
    </HStack>
  );
}

/** One group's rows: Default, then the catalog. `selection` is `null` when the pick lies in another group. */
function GroupChoices({ group, selection, onChange }: Readonly<{
  group: ModelGroup;
  selection: ModelSelection | null;
  onChange: (next: ModelSelection) => void;
}>) {
  const models: readonly CatalogEntry[] = group.catalog?.models ?? [];
  const defaultName = defaultNameOf(group.catalog);
  const unreachable = group.catalog !== null && group.catalog.source === 'unavailable';
  /* A provider that cannot run now offers nothing to pick, and says why in the server's own words. */
  const blocked = group.availability?.status === 'unavailable' ? group.availability.reason : null;
  return <>
    {blocked !== null && (
      <DropdownMenuItem label={`${PROVIDERS[group.provider].label} is unavailable`} description={blocked} isDisabled />
    )}
    <Choice
      label={defaultName === null ? FOLLOW_DEFAULT_LABEL : `${FOLLOW_DEFAULT_LABEL} (${defaultName})`}
      isSelected={selection !== null && selection.model === null}
      isDisabled={blocked !== null}
      onSelect={() => onChange({ model: null, reasoning_effort: null })}
    />
    {models.map((model) => (
      <Choice
        key={model.id}
        label={model.display_name}
        isSelected={selection !== null && selection.model === model.model}
        isDisabled={blocked !== null}
        /* Switching model drops the effort: one chosen for the previous model may not exist on this one. */
        onSelect={() => onChange({ model: model.model, reasoning_effort: null })}
      />
    ))}
    {models.length === 0 && blocked === null && (
      <DropdownMenuItem
        label={unreachable ? PROVIDERS[group.provider].unavailable : 'No models available on this account'}
        isDisabled
      />
    )}
  </>;
}

function EffortPill({
  efforts, value, defaultName, onChange, placement, isDisabled,
}: Readonly<{
  efforts: CatalogEntry['supported_reasoning_efforts'];
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
  efforts: CatalogEntry['supported_reasoning_efforts'];
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
