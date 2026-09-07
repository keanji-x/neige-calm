// The model picker that sits in a planner conversation's composer footer
// (#1505 S4-3/S4-4).
//
// Presentational on purpose: everything it shows is a prop. The queries and the
// write live in `app/router`, because `features/**` may not import `app/**` and
// because the pill has no business owning a conversation's data.
//
// ## Two levels, and why the second one sometimes is not there
//
// Reasoning effort is a property *of a model*: each catalog entry carries its
// own `supported_reasoning_efforts`, and the descriptions beside them are
// codex's own words, never ours. A model that offers one effort (or none) gets
// no effort control at all — a menu with a single choice is a control that
// cannot be operated.
//
// ## What "Default" means here, precisely
//
// It means "follow whatever this installation is configured to use", not the
// name of a model. When we know that name it is shown beside the word; when we
// do not — no card workspace to resolve layers against, an unreadable config —
// the word stands alone rather than borrowing a name from somewhere weaker.
// `GET /api/models` reports which of those two happened in `default_source`,
// and the caller passes it through.

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

/**
 * The one sentence about switching models that is true every time.
 *
 * Deliberately a standing note in the menu rather than a warning attached to a
 * particular change: whether a switch actually triggers a compaction depends on
 * the two models' context windows and on how full the thread already is, and
 * the catalog we are given carries no window at all. A caption that asserted it
 * on every switch would be false in the great majority of them, and a warning
 * that is usually false is worse than none.
 */
const SWITCH_NOTE = 'Switching to a model with a smaller context window can make codex compact the history first.';

export function ModelPill({
  catalog, selection, onChange, isDisabled = false, placement = 'above', triggerId,
}: Readonly<{
  /** What can be chosen, and what the installation follows. `null` while it loads. */
  catalog: ModelCatalog | null;
  /** What this conversation has chosen. `null` in both fields is "follow the default". */
  selection: ModelSelection;
  onChange: (next: ModelSelection) => void;
  isDisabled?: boolean;
  placement?: 'above' | 'below';
  triggerId?: string;
}>) {
  const [open, setOpen] = useState(false);
  const hostRef = useRef<HTMLSpanElement | null>(null);

  const models = catalog?.models ?? [];
  const unreachable = catalog !== null && catalog.source === 'unavailable';
  const defaultName = catalog?.default_source === 'config_read' || catalog?.default_source === 'config_toml'
    ? catalog.default.model
    : null;
  /*
   * Which catalog entry the effort control is about.
   *
   * Following the installation default is still running a MODEL, and that
   * model's efforts are sitting right there in the catalog — so the control
   * belongs here too. This used to resolve to `undefined` whenever
   * `selection.model` was null, which is every conversation that has not
   * overridden anything, i.e. all of them until someone touches the pill: the
   * effort menu simply did not exist for the common case. Verified against the
   * kernel rather than assumed — `catalog_advice` (`routes/planner_model.rs`)
   * takes `{model: null, reasoning_effort: "high"}` and stores it unjudged,
   * with an explicit note that with no model chosen there is no catalog entry
   * to judge the effort against.
   *
   * That note is also the honest limit on what this list means while the
   * default is being followed: these are the efforts of whatever the default
   * resolves to NOW. If the installation's default model changes under a
   * conversation, the stored effort travels with it and the kernel decides at
   * turn time. We are not promising otherwise.
   */
  const followed = catalog?.default.model == null
    ? undefined
    : models.find((model) => model.model === catalog.default.model);
  const chosen = selection.model === null
    ? followed
    : models.find((model) => model.model === selection.model);
  /*
   * The trigger says the MODEL, not how it was arrived at.
   *
   * It used to read `Default (gpt-6-astra)`, and the first word was noise on
   * every conversation nobody has touched: what a person wants off that pill
   * is which model is running, and "Default" is a fact about the *route* to
   * that answer, not the answer. The route still has a place — the menu's
   * first row is still `Default (gpt-6-astra)`, where the word is what
   * distinguishes "follow whatever this installation uses" from pinning that
   * same model by name, and the tick says which of the two is in force.
   *
   * The one case where the word survives here is the one where dropping it
   * would leave nothing true to say: no catalog, or a default this
   * installation cannot resolve. Then `Default` alone IS the whole of what is
   * known.
   *
   * A slug we hold that the catalog does not list still names the model this
   * conversation runs; showing the slug is more use than showing nothing.
   */
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
    // Owned here for the same reason the starting-point pill owns it: a host
    // Dialog's document listener would otherwise take Escape first and the
    // trigger would not get its focus back.
    event.preventDefault();
    event.stopPropagation();
    setOpen(false);
    requestAnimationFrame(() => hostRef.current?.querySelector('button')?.focus());
  };

  return (
    <HStack gap={1} align="center">
      <span ref={hostRef} className={styles.host} onKeyDownCapture={closeOnEscape}>
        <DropdownMenu
          placement={placement}
          isMenuOpen={open}
          onOpenChange={setOpen}
          /*
           * No chevron, at the owner's call. Worth naming what that spends:
           * with no fill, no border and now no glyph, nothing about this
           * control announces itself as one until the pointer is over it —
           * hover and focus are the whole of the affordance. It keeps its
           * button role and its name, so nothing is lost to a screen reader or
           * to the keyboard; what a mouse loses is the hint that there is
           * something here to press.
           */
          hasChevron={false}
          button={{
            id: triggerId,
            label: spokenLabel,
            /* Model names run long — `gpt-5.1-codex-max` and worse — and the
               trigger cannot have the whole footer. `maxLines` ends it in an
               ellipsis and `hasTruncateTooltip` offers the full name on hover
               ONLY when it was actually shortened, which is the difference
               between this and the `max-inline-size` that used to just cut it
               off with no way to read the rest. */
            /* `type`/`color` inherit, and that is load-bearing: `Text`
               defaults to body size in primary text and would re-assert both
               over the trigger's own — measured, the model name stayed large
               and black while the effort beside it (a plain string, so it
               inherits) went small and grey. Same shape as the tooltip bug in
               `context-ring.tsx`: `Text` on somebody else's surface has to be
               told to inherit. */
            children: (
              <Text type="inherit" color="inherit" maxLines={1} hasTruncateTooltip>
                {label}
              </Text>
            ),
            /* `ghost`: no fill, no border. The composer footer is a quiet row
               under the field, and a filled pill there was the heaviest thing
               in it — heavier than Send, which is the control anyone looking
               at that row is actually aiming for. With the chevron gone too
               (see `hasChevron` above), hover and focus are all that is left
               to say it is pressable. */
            variant: 'ghost',
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
              /* Switching model drops the effort back to "follow the default":
                 an effort chosen for the previous model may not exist on this
                 one, and carrying it over is how a selection the server has to
                 quietly correct gets made. */
              onSelect={() => onChange({ model: model.model, reasoning_effort: null })}
            />
          ))}
          {models.length === 0 && (
            <DropdownMenuItem
              label={unreachable ? 'codex is not running' : 'No models available on this account'}
              isDisabled
            />
          )}
          <Divider />
          <div className={styles.note} role="note">
            <Text type="supporting">{SWITCH_NOTE}</Text>
          </div>
        </DropdownMenu>
      </span>
      {efforts.length > 1 && (
        <EffortPill
          efforts={efforts}
          value={selection.reasoning_effort}
          /* The name behind the word "Default", when there is one to give —
             the same treatment the model trigger gets, and for the same
             reason: "Default" alone tells you that you have not chosen, not
             what you are getting. While a model IS chosen the entry's own
             `default_reasoning_effort` is the one that applies; while the
             installation default is followed it is the catalog's. */
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
          /* Both triggers are ghost now that neither is filled, so the
             subordination that used to come from `secondary` vs `ghost` comes
             from colour instead (`.effort`). Effort is a property OF the
             chosen model and has to read as one; two identical controls side
             by side said they were two independent settings, which is the one
             thing this pair is not. */
          variant: 'ghost',
          size: 'sm',
          isDisabled,
          className: styles.effort,
        }}
      >
        <Choice
          label={defaultName === null
            ? FOLLOW_DEFAULT_LABEL : `${FOLLOW_DEFAULT_LABEL} (${defaultName})`}
          isSelected={value === null}
          onSelect={() => onChange(null)}
        />
        {efforts.map((effort) => (
          <Choice
            key={effort.reasoning_effort}
            label={effort.reasoning_effort}
            /* codex's own wording, passed through. We do not write copy for
               somebody else's setting. */
            description={effort.description}
            isSelected={value === effort.reasoning_effort}
            onSelect={() => onChange(effort.reasoning_effort)}
          />
        ))}
      </DropdownMenu>
    </span>
  );
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
