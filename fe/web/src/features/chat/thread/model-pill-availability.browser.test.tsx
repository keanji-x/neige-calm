// The new-track picker's unavailable group in a real browser (#1817): the server's reason is readable in
// the open menu at phone width, and nothing in that group can be picked.
import '../../../styles/entry.css';
import { render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { expect, it, vi } from 'vitest';
import { page } from 'vitest/browser';

import type { ProviderAvailability } from '../../../../../core/domain/agent-providers.ts';
import { FOLLOW_INSTALLATION_DEFAULT, type ModelCatalog } from '../../../../../core/domain/conversation.ts';
import { ModelPill } from './model-pill.tsx';

const REASON = 'not logged in — run `claude /login` with CLAUDE_CONFIG_DIR=/home/owner/.local/share/claude-planner';

/** A live catalog; a Claude one (#1822) reports each entry's resolved model and no default effort. */
function catalog(models: readonly [string, string][], provider: 'codex' | 'claude'): ModelCatalog {
  return {
    models: models.map(([model, name]) => ({ id: model, model, resolved_model: provider === 'claude' ? `claude-${model}` : null,
      display_name: name, description: '', is_default: false,
      supported_reasoning_efforts: [], default_reasoning_effort: provider === 'claude' ? null : 'low' })),
    default: { model: null, reasoning_effort: null, supported_reasoning_efforts: null }, default_source: 'unknown',
    source: 'live', fetched_at_ms: 1,
  };
}

const ready: ProviderAvailability = { provider: 'codex', status: 'ready', reason: null, checked_at_ms: 1 };
const claudeUnavailable: ProviderAvailability = { provider: 'claude', status: 'unavailable', reason: REASON, checked_at_ms: 1 };

it('shows why Claude cannot run, inside the viewport, and picks nothing from its group', async () => {
  await page.viewport(390, 844);
  const onChange = vi.fn();
  render(<ModelPill provider="codex" effortControl="in-menu" selection={FOLLOW_INSTALLATION_DEFAULT} onChange={onChange}
    groups={[
      { provider: 'codex', availability: ready, catalog: catalog([['gpt-5', 'GPT-5']], 'codex') },
      { provider: 'claude', availability: claudeUnavailable, catalog: catalog([['opus[1m]', 'Opus'], ['sonnet', 'Sonnet']], 'claude') },
    ]} />);
  await userEvent.click(screen.getByRole('button', { name: /^Model:/ }));
  const claude = await screen.findByRole('group', { name: 'Claude' });
  const notice = within(claude).getByRole('note');
  await expect.element(notice).toBeVisible();
  expect(notice.textContent).toContain(REASON);
  const bounds = notice.getBoundingClientRect();
  expect(bounds.left).toBeGreaterThanOrEqual(0);
  expect(bounds.right).toBeLessThanOrEqual(390);
  await userEvent.click(within(claude).getByRole('menuitem', { name: 'Opus' }), { force: true } as never);
  expect(onChange).not.toHaveBeenCalled();
  await page.screenshot({ path: '../../../../../test-results/model-pill-claude-unavailable.png' });
});
