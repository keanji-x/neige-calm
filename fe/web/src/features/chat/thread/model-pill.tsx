// The model picker in a planner conversation's composer footer, and on the new-track page. Presentational:
// every value is a prop; the queries and the write live in `app/router`. Each provider's catalog is one
// group; on the new-track page the pick also decides the Planner's provider (#1810), and each group
// follows its provider's availability (#1817). A Claude group is the Claude CLI's own list (#1822).
// Beside it, a Planner conversation's approval setting (#2348), and the Planner's role on the new-track page and in
// an existing Planner conversation, triggers of the same kind.

import { DropdownMenu, DropdownMenuItem } from '@astryxdesign/core/DropdownMenu';
import { Divider } from '@astryxdesign/core/Divider';
import { HStack } from '@astryxdesign/core/HStack';
import { Icon as AstryxIcon } from '@astryxdesign/core/Icon';
import { Text } from '@astryxdesign/core/Text';
import { VisuallyHidden } from '@astryxdesign/core/VisuallyHidden';
import { Fragment, useRef, type KeyboardEvent, type ReactNode, type RefObject } from 'react';

import type { AgentProvider, PlannerPermissionMode } from '../../../../../core/api/generated/wire.ts';
import {
  CREATE_REFUSED_WHEN_UNAVAILABLE, STILL_CREATES_NOTE, type ProviderAvailability,
} from '../../../../../core/domain/agent-providers.ts';
import type { ModelCatalog, ModelSelection } from '../../../../../core/domain/conversation.ts';
import { useState } from '../../../ui/state/public.ts';
import styles from './model-pill.module.css';

const FOLLOW_DEFAULT_LABEL = 'Default';

/** A standing note rather than a per-switch warning: the catalog carries no context window, so whether a switch compacts is unknowable here. */
const SWITCH_NOTE = 'Switching to a model with a smaller context window can make codex compact the history first.';

/**
 * Legacy Planner presentation only; managed groups use the declared provider identifier.
 * Total over AgentProvider, so a new backend is a compile error until its Planner presentation is declared.
 * `unavailable` says why that provider's `source: 'unavailable'` catalog is empty. A Claude group waits for
 * its availability and catalog before it joins a menu that offers other providers (`hiddenUntilKnown`); a
 * Codex one stays while the daemon is down, saying so. Only codex compacts (`switchNote`).
 * Whether an unavailable group stays pickable is core's `CREATE_REFUSED_WHEN_UNAVAILABLE` (#1817), the
 * one per-provider flag Settings reads too.
 */
const PROVIDERS: Readonly<Record<AgentProvider, Readonly<{
  label: string; unavailable: string; hiddenUntilKnown: boolean; switchNote: boolean;
}>>> = Object.freeze({
  codex: Object.freeze({
    label: 'Codex', unavailable: 'codex is not running', hiddenUntilKnown: false, switchNote: true,
  }),
  claude: Object.freeze({
    label: 'Claude', unavailable: 'Claude cannot run on this server right now', hiddenUntilKnown: true,
    switchNote: false,
  }),
  opencode: Object.freeze({
    label: 'OpenCode', unavailable: 'OpenCode cannot run on this server right now', hiddenUntilKnown: true,
    switchNote: false,
  }),
});

/**
 * One provider's section of the menu: its catalog, `null` while it loads, and whether the provider can run
 * right now (`GET /api/agent-providers`). `availability` is `null` while that answer is unknown, and for a
 * conversation that already exists: its turns keep issue-time handling (#1817).
 */
export type ModelGroup<Provider extends string = AgentProvider> = Readonly<{
  provider: Provider;
  catalog: ModelCatalog | null;
  availability: ProviderAvailability | null;
  /** Explicit host admission, independent of the Planner create policy; undefined keeps its existing behavior. */
  selectionProblem?: string | null;
}>;

function legacyPresentation(provider: string) {
  return Object.entries(PROVIDERS).find(([key]) => key === provider)?.[1];
}

/** Managed display needs no provider-specific UI registration. */
function providerLabel<Provider extends string>(group: ModelGroup<Provider> | undefined, provider: Provider): string {
  return group?.selectionProblem !== undefined ? provider : (legacyPresentation(provider)?.label ?? provider);
}

type CatalogEntry = ModelCatalog['models'][number];

/**
 * The groups a menu shows. The selection's own group always shows, so a pick never passes for another
 * provider's. Any other group is hidden while its provider is `not_configured` on this server.
 */
function visibleModelGroups<Provider extends string>(groups: readonly ModelGroup<Provider>[], provider: Provider): readonly ModelGroup<Provider>[] {
  return groups.filter((group) => group.selectionProblem !== undefined || group.provider === provider || (
    group.availability?.status !== 'not_configured'
    && (!legacyPresentation(group.provider)?.hiddenUntilKnown || (group.availability !== null && group.catalog !== null))));
}

/**
 * Escape closes an open menu and returns focus to its trigger. A host Dialog's document listener would otherwise take
 * Escape first and the trigger would not get its focus back.
 */
function closeOnEscape(open: boolean, setOpen: (open: boolean) => void, host: RefObject<HTMLSpanElement | null>) {
  return (event: KeyboardEvent<HTMLSpanElement>) => {
    if (event.key !== 'Escape' || !open) return;
    event.preventDefault();
    event.stopPropagation();
    setOpen(false);
    requestAnimationFrame(() => host.current?.querySelector('button')?.focus());
  };
}

/** The name of the default a catalog says is followed (for Claude, the model its CLI default resolves to), or `null` when it cannot say. */
function defaultNameOf(catalog: ModelCatalog | null): string | null {
  return catalog?.default_source === 'config_read' || catalog?.default_source === 'config_toml'
    || catalog?.default_source === 'claude_cli' || catalog?.default_source === 'acp_session'
    ? catalog.default.model
    : null;
}

export function ModelPill<Provider extends string = AgentProvider>({
  groups, provider, selection, onChange, isDisabled = false, placement = 'above', triggerId, effortControl = 'separate',
}: Readonly<{
  /** What can be chosen, one group per provider, in menu order. A conversation passes only its own provider's. */
  groups: readonly ModelGroup<Provider>[];
  /** The provider `selection` belongs to. */
  provider: Provider;
  /** What this conversation has chosen. `null` in both fields is "follow the default". */
  selection: ModelSelection;
  /** A pick in another group also hands back that group's provider. */
  onChange: (next: ModelSelection, provider: Provider) => void;
  isDisabled?: boolean;
  placement?: 'above' | 'below';
  triggerId?: string;
  /** A narrow host can keep model and effort inside a single menu. */
  effortControl?: 'separate' | 'in-menu';
}>) {
  const [open, setOpen] = useState(false);
  const hostRef = useRef<HTMLSpanElement | null>(null);

  const shown = visibleModelGroups(groups, provider);
  const grouped = shown.length > 1 || shown.some(group => group.selectionProblem !== undefined);
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
  /* A Claude default resolves to a model a listed entry also runs (#1822): that entry's name is the one to show. */
  const defaultEntry = defaultName === null ? undefined : models.find((model) => model.resolved_model === defaultName);
  /* The trigger names the model, not the route to it; `Default` alone only when nothing truer can be said, and an unlisted slug still names what runs. */
  const named = selection.model === null
    ? (defaultEntry?.display_name ?? defaultName ?? FOLLOW_DEFAULT_LABEL)
    : (chosen?.display_name ?? selection.model);
  /* With more than one provider on offer, the pick is a provider too, and the trigger says whose. */
  const label = grouped ? `${providerLabel(groups.find(group => group.provider === provider), provider)} ${named}` : named;
  /* The accessible name keeps what the visible one dropped. A person reading
     the pill has the menu one press away; a person hearing it does not. */
  const spokenLabel = selection.model === null && defaultName !== null
    ? `Model: ${label} (this installation's default)`
    : `Model: ${label}`;

  /* A Claude default is no row of its own: the catalog's `default` carries the efforts it takes. */
  const efforts = (selection.model === null ? catalog?.default.supported_reasoning_efforts : null)
    ?? chosen?.supported_reasoning_efforts ?? [];
  const effortDefault = selection.model === null
    ? catalog?.default.reasoning_effort ?? null
    : chosen?.default_reasoning_effort ?? null;
  const switchNote = shown.some((group) => group.selectionProblem === undefined && legacyPresentation(group.provider)?.switchNote);

  return (
    <HStack gap={1} align="center" className={styles.group}>
      <span ref={hostRef} className={styles.host} onKeyDownCapture={closeOnEscape(open, setOpen, hostRef)}>
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
            isDisabled: isDisabled || (unreachable && !shown.some(group => group.selectionProblem === null)),
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
                <div role="group" aria-label={providerLabel(group, group.provider)}>
                  <div className={styles.groupHeading} aria-hidden="true">{providerLabel(group, group.provider)}</div>
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
function GroupChoices<Provider extends string>({ group, selection, onChange }: Readonly<{
  group: ModelGroup<Provider>;
  selection: ModelSelection | null;
  onChange: (next: ModelSelection) => void;
}>) {
  const models: readonly CatalogEntry[] = group.catalog?.models ?? [];
  const defaultName = defaultNameOf(group.catalog);
  const unreachable = group.catalog !== null && group.catalog.source === 'unavailable';
  /* A provider that cannot run now says why in the server's own words; one create refuses then offers
     nothing to pick, and one create still accepts stays pickable under a warning. */
  const reason = group.selectionProblem === undefined
    ? (group.availability?.status === 'unavailable' ? group.availability.reason : null) : group.selectionProblem;
  const refused = group.selectionProblem !== undefined || Object.entries(CREATE_REFUSED_WHEN_UNAVAILABLE).find(([key]) => key === group.provider)?.[1];
  const blocked = refused ? reason : null;
  return <>
    {group.selectionProblem !== undefined && group.availability?.authentication_notice != null && (
      <div className={styles.reason} role="note"><Text type="supporting">{group.availability.authentication_notice.text}</Text></div>
    )}
    {reason !== null && (
      <div className={refused ? styles.reason : `${styles.reason} ${styles.warning}`} role="note">
        <Text type="supporting" color={refused ? undefined : 'inherit'}>
          {`${providerLabel(group, group.provider)} is unavailable: ${reason}${refused ? '' : ` ${STILL_CREATES_NOTE}`}`}
        </Text>
      </div>
    )}
    <Choice
      /* A Claude default is described by the CLI's own entry: "Default", and the model it resolves to beside it. */
      label={group.catalog?.default_source === 'acp_session' ? `Session settings (${defaultName ?? 'current'})`
        : defaultName === null ? FOLLOW_DEFAULT_LABEL
        : group.catalog?.default_source === 'claude_cli' ? <ResolvedLabel name={FOLLOW_DEFAULT_LABEL} resolved={defaultName} />
          : `${FOLLOW_DEFAULT_LABEL} (${defaultName})`}
      isSelected={selection !== null && selection.model === null}
      isDisabled={blocked !== null}
      onSelect={() => onChange({ model: null, reasoning_effort: null })}
    />
    {models.map((model) => (
      <Choice
        key={model.id}
        /* What the entry runs, where its provider says (a Claude entry's resolved model, #1822). */
        label={model.resolved_model === null ? model.display_name
          : <ResolvedLabel name={model.display_name} resolved={model.resolved_model} />}
        isSelected={selection !== null && selection.model === model.model}
        isDisabled={blocked !== null}
        /* Switching model drops the effort: one chosen for the previous model may not exist on this one. */
        onSelect={() => onChange({ model: model.model, reasoning_effort: null })}
      />
    ))}
    {models.length === 0 && reason === null && (
      <DropdownMenuItem
        label={unreachable ? (group.selectionProblem !== undefined ? `${group.provider} model catalog is unavailable`
          : legacyPresentation(group.provider)?.unavailable ?? `${group.provider} is unavailable`) : 'No models available on this account'}
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
  return (
    <span ref={hostRef} className={styles.host} onKeyDownCapture={closeOnEscape(open, setOpen, hostRef)}>
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

/** Total over `PlannerPermissionMode`, in menu order, so a new mode is a compile error here rather than a missing row. */
const PERMISSION_MODES: Readonly<Record<PlannerPermissionMode, Readonly<{ label: string; description: string }>>> = Object.freeze({
  never: Object.freeze({ label: 'Never', description: 'Sandbox only; never asks.' }),
  ask: Object.freeze({ label: 'Ask me', description: 'Pauses the turn to ask you.' }),
  full: Object.freeze({ label: 'Full access', description: "No sandbox; never asks. Runs with your account's access." }),
});

/** The kernel reads the mode when it starts a turn, so a change never reaches the one already running. */
const PERMISSION_NOTE = 'Applies from the next turn.';

/** A Planner conversation's approval setting (#2348, #2441): whether its turns stay in the sandbox, may pause to ask before leaving it, or run without one. */
export function PermissionModePill({ mode, onChange, isDisabled = false, placement = 'above' }: Readonly<{
  /** What the card has stored. */
  mode: PlannerPermissionMode;
  onChange: (mode: PlannerPermissionMode) => void;
  isDisabled?: boolean;
  placement?: 'above' | 'below';
}>) {
  return <SettingPill name="Approvals" options={PERMISSION_MODES} value={mode} onChange={onChange}
    note={PERMISSION_NOTE} isDisabled={isDisabled} placement={placement} />;
}

/** What a Track's Planner may do in its Area: an ordinary Planner, or an Area admin (`admin` on `POST`/`PATCH /api/tracks`). */
export type PlannerRole = 'member' | 'admin';

/** Total over `PlannerRole`, in menu order; Member is the default. */
const PLANNER_ROLES: Readonly<Record<PlannerRole, Readonly<{ label: string; description: string }>>> = Object.freeze({
  member: Object.freeze({ label: 'Member', description: 'An ordinary Planner.' }),
  admin: Object.freeze({ label: 'Admin', description: 'May add Tracks and close the ones it added.' }),
});

/** The Planner's role beside a model picker, on the new-track page and in an existing Track's Planner conversation: a trigger of the approval setting's kind. */
export function PlannerRolePill({ value, onChange, isDisabled = false, disabledReason, placement = 'above' }: Readonly<{
  /** The role chosen; `role` would read as an ARIA role. */
  value: PlannerRole;
  onChange: (role: PlannerRole) => void;
  isDisabled?: boolean;
  /** Why it cannot be changed, as the disabled trigger's tooltip; it still names the current role. */
  disabledReason?: string;
  placement?: 'above' | 'below';
}>) {
  /* It sits at the trailing edge beside Create, so its menu opens leftward: room for its descriptions on a phone. */
  return <SettingPill name="Role" options={PLANNER_ROLES} value={value} onChange={onChange}
    isDisabled={isDisabled} tooltip={isDisabled ? disabledReason : undefined} placement={placement} alignment="end" />;
}

/** A short fixed setting: the trigger names it and its value, and each row explains itself. */
function SettingPill<Value extends string>({
  name, options, value, onChange, note, isDisabled, tooltip, placement, alignment = 'start',
}: Readonly<{
  name: string;
  options: Readonly<Record<Value, Readonly<{ label: string; description: string }>>>;
  value: Value;
  onChange: (value: Value) => void;
  note?: string;
  isDisabled: boolean;
  /** With it Astryx marks a disabled trigger `aria-disabled` rather than `disabled`, so the tooltip stays reachable. */
  tooltip?: string;
  placement: 'above' | 'below';
  alignment?: 'start' | 'end';
}>) {
  const [open, setOpen] = useState(false);
  const hostRef = useRef<HTMLSpanElement | null>(null);
  const label = `${name}: ${options[value].label}`;
  return (
    <span ref={hostRef} className={styles.host} onKeyDownCapture={closeOnEscape(open, setOpen, hostRef)}>
      <DropdownMenu
        placement={placement}
        alignment={alignment}
        /* An `aria-disabled` trigger still passes ArrowDown to the menu, so a disabled one is held shut here. */
        isMenuOpen={open && !isDisabled}
        onOpenChange={(next) => { setOpen(next && !isDisabled); }}
        hasChevron={false}
        button={{
          label,
          children: <Text type="inherit" color="inherit" maxLines={1}>{label}</Text>,
          variant: 'ghost',
          size: 'sm',
          isDisabled,
          tooltip,
          className: styles.setting,
        }}
      >
        {(Object.keys(options) as Value[]).map((option) => (
          /* A node, not a string: Astryx cuts a string description to one line, and Full access's says what it risks. */
          <Choice key={option} label={options[option].label}
            description={<Text type="inherit" color="inherit">{options[option].description}</Text>}
            isSelected={value === option} onSelect={() => { if (option !== value) onChange(option); }} />
        ))}
        {note !== undefined && (
          <>
            <Divider />
            <div className={styles.settingNote} role="note">
              <Text type="supporting">{note}</Text>
            </div>
          </>
        )}
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
        description={effort.description ?? undefined} isSelected={value === effort.reasoning_effort}
        onSelect={() => onChange(effort.reasoning_effort)} />
    ))}
  </>;
}

/**
 * One line (owner layout, 2026-09-28): the entry's name, and the model it resolves to small and quiet at the
 * right, which gives way with an ellipsis first when the menu runs out of room.
 */
function ResolvedLabel({ name, resolved }: Readonly<{ name: string; resolved: string }>) {
  return (
    <span className={styles.resolvedRow}>
      <span className={styles.resolvedName}>{name}</span>
      <span className={styles.resolvedId} title={resolved}>{resolved}</span>
    </span>
  );
}

function Choice({
  label, description, isSelected, isDisabled = false, onSelect,
}: Readonly<{
  label: ReactNode;
  description?: ReactNode;
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
