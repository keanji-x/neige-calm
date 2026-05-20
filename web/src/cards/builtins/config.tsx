// Transient "config card" — a registry entry the AddPanel pipeline drops
// into the Wave grid while the user fills in a SchemaForm. Never sourced
// from a kernel card (no `fromKernel`). The Wave page owns its lifetime:
// the card disappears once the user submits or cancels.
//
// Lives as a registered CardEntry only so the existing WaveCard /
// WaveGrid pipeline (sizing, drag handle, close button) renders it with
// zero extra plumbing. The behaviour difference vs. real cards is just
// that nothing posts to the kernel until the form is submitted.

import { useMemo } from 'react';
import { SchemaForm } from '../../shared/components/SchemaForm';
import { addPanelEntries, type CardEntry } from '../registry';
import type { ConfigCardData } from '../../types';

/**
 * The Wave page sets these callbacks on a module-level ref keyed by
 * config-card id before pushing the card into the slot list. The
 * component reads them by id. This avoids piping a non-card-shaped prop
 * through `renderCard`, which only knows about the discriminated union.
 */
const HANDLERS = new Map<string, ConfigCardHandlers>();

export interface ConfigCardHandlers {
  onSubmit: (values: Record<string, string>) => void | Promise<void>;
  onCancel: () => void;
}

export function registerConfigCardHandlers(id: string, h: ConfigCardHandlers): () => void {
  HANDLERS.set(id, h);
  return () => {
    HANDLERS.delete(id);
  };
}

function ConfigCard({ card }: { card: ConfigCardData }) {
  const entry = useMemo(
    () => addPanelEntries().find((e) => e.type === card.targetKind),
    [card.targetKind],
  );
  const handlers = card.id ? HANDLERS.get(card.id) : undefined;

  if (!entry?.createSchema) {
    return (
      <div className="config-card">
        <div className="config-card-head card-drag-handle">
          <span className="config-card-title">Configure</span>
        </div>
        <div className="config-card-body">
          <p>No schema registered for kind “{card.targetKind}”.</p>
        </div>
      </div>
    );
  }

  return (
    <div className="config-card">
      <div className="config-card-head card-drag-handle">
        <span className="config-card-title">New {entry.label.replace(/^New\s+/i, '')}</span>
      </div>
      <div className="config-card-body">
        <SchemaForm
          schema={entry.createSchema}
          submitLabel="Create"
          onSubmit={(v) => handlers?.onSubmit(v)}
          onCancel={() => handlers?.onCancel()}
        />
      </div>
    </div>
  );
}

export const ConfigCardEntry: CardEntry<ConfigCardData> = {
  type: 'config',
  Component: ConfigCard,
  defaultSize: { w: 5, h: 8, minW: 4, minH: 5 },
  // No `fromKernel` — config cards are local-only.
};
