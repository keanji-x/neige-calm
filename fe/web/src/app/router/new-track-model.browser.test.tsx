// Exercise the app's composition of the Area form and Chat model control
// with the production styles and a real narrow-screen browser layout.
import '../../styles/entry.css';
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { expect, it, vi } from 'vitest';
import { page } from 'vitest/browser';
import { FOLLOW_INSTALLATION_DEFAULT, type ModelCatalog, type ModelSelection } from '../../../../core/domain/conversation.ts';
import { NewTrackForm } from '../../features/area/new-track/public.tsx';
import { ModelPill } from '../../features/chat/thread/model-pill.tsx';
import { useState } from '../../ui/state/public.ts';

function ModelForm() {
  const [selection, setSelection] = useState<ModelSelection>(FOLLOW_INSTALLATION_DEFAULT);
  const catalog: ModelCatalog = {
    models: [{ id: 'test-model', model: 'test-model', display_name: 'Test model', description: '',
      is_default: true, default_reasoning_effort: 'low', supported_reasoning_efforts: [
        { reasoning_effort: 'low', description: 'Faster' },
        { reasoning_effort: 'high', description: 'More reasoning' },
      ] }],
    default: { model: null, reasoning_effort: null }, default_source: 'unknown',
    source: 'live', fetched_at_ms: 1,
  };
  return <NewTrackForm submitting={false} error={null} templates={[]} templatesLoaded
    initialTemplateId={null} initialCwd={null} onManageRecipes={vi.fn()}
    listDirectory={vi.fn()} onSubmit={vi.fn()}
    modelControls={<ModelPill catalog={catalog} selection={selection} onChange={setSelection} />}
  />;
}

it('places working model and effort menus beside Send without overflowing a narrow composer', async () => {
  await page.viewport(390, 844);
  const view = render(<ModelForm />);
  try {
    await userEvent.click(screen.getByRole('button', { name: 'Model: Default' }));
    await userEvent.click(screen.getByRole('menuitem', { name: 'Test model' }));
    await userEvent.click(screen.getByRole('button', { name: 'Reasoning effort: low (the default)' }));
    await userEvent.click(screen.getByRole('menuitem', { name: /high/ }));
    const model = screen.getByRole('button', { name: 'Model: Test model' }).getBoundingClientRect();
    const effort = screen.getByRole('button', { name: 'Reasoning effort: high' }).getBoundingClientRect();
    const send = screen.getByRole('button', { name: 'Create track' }).getBoundingClientRect();
    expect(model.right).toBeLessThanOrEqual(effort.left);
    expect(effort.right).toBeLessThanOrEqual(send.left);
    expect(Math.abs(effort.top - send.top)).toBeLessThan(10);
    expect(send.right).toBeLessThanOrEqual(window.innerWidth);
    expect(document.documentElement.scrollWidth).toBe(window.innerWidth);
  } finally {
    view.unmount();
    await page.viewport(1280, 720);
  }
});
