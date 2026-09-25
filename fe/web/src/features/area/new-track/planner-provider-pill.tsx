// Which backend runs the new track's Planner. Presentational: the choice lives in the
// caller's draft, which also owns what a switch resets.

import { DropdownMenu, DropdownMenuItem } from '@astryxdesign/core/DropdownMenu';
import { Icon } from '@astryxdesign/core/Icon';
import { VisuallyHidden } from '@astryxdesign/core/VisuallyHidden';

import type { AgentProvider } from '../../../../../core/api/generated/wire.ts';
import { useState } from '../../../ui/state/public.ts';
import styles from './planner-provider-pill.module.css';

/** Total over `AgentProvider`, so a new backend is a compile error here rather than a missing row. */
const CHOICES: Readonly<Record<AgentProvider, Readonly<{ label: string; description: string }>>> = Object.freeze({
  codex: Object.freeze({ label: 'Codex', description: 'Choose a model and effort.' }),
  claude: Object.freeze({ label: 'Claude', description: 'Runs Claude Code’s default model and effort.' }),
});

const ORDER: readonly AgentProvider[] = Object.freeze(['codex', 'claude'] as const);

export function PlannerProviderPill({ value, onChange, isDisabled, variant }: Readonly<{
  value: AgentProvider;
  onChange: (next: AgentProvider) => void;
  isDisabled: boolean;
  /** Matches the model control beside it: filled where it is, quiet where it is not. */
  variant: 'ghost' | 'secondary';
}>) {
  const [open, setOpen] = useState(false);
  const { label } = CHOICES[value];
  return (
    <DropdownMenu
      placement="above"
      isMenuOpen={open}
      onOpenChange={setOpen}
      hasChevron={false}
      button={{
        label: `Planner: ${label}`,
        children: label,
        variant,
        size: 'sm',
        isDisabled,
        className: styles.trigger,
      }}
    >
      {ORDER.map((provider) => (
        <DropdownMenuItem
          key={provider}
          label={CHOICES[provider].label}
          description={CHOICES[provider].description}
          onClick={() => { if (provider !== value) onChange(provider); }}
          endContent={provider === value ? (
            <>
              <Icon icon="check" size="sm" color="accent" />
              <VisuallyHidden>Selected</VisuallyHidden>
            </>
          ) : undefined}
        />
      ))}
    </DropdownMenu>
  );
}
