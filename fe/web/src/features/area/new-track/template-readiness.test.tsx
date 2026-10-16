import { cleanup, render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, it, vi } from 'vitest';
import { NewTrackForm } from './public.tsx';

afterEach(cleanup);
const TEMPLATE = Object.freeze({
  id: 'site/custom-workflow', title: 'Custom workflow', description: 'Turn a request into a reviewed result.',
  availability: { status: 'requires_plugin' as const, plugin_id: 'custom-owner', plugin_name: 'Workflow tools' },
  tasks: [],
});

it('explains a template and its missing prerequisite before selection and blocks creation until ready', async () => {
  const onSubmit = vi.fn();
  const onOpenPluginSettings = vi.fn();
  const props = {
    submitting: false, error: null, templates: [TEMPLATE], templatesLoaded: true,
    initialTemplateId: null, initialCwd: null, onManageRecipes: vi.fn(),
    loadTemplate: (id: string) => Promise.resolve({ id, title: TEMPLATE.title, description: TEMPLATE.description, instructions: null, body: '# Result' }),
    listDirectory: vi.fn(), onSubmit, onOpenPluginSettings,
  };
  const view = render(<NewTrackForm {...props} />);
  await userEvent.type(screen.getByLabelText('What this track should do'), 'Fix the thing');
  await userEvent.click(screen.getByRole('button', { name: 'Template: No template' }));
  const menu = await screen.findByRole('menu');
  expect(within(menu).getByText(TEMPLATE.description)).toBeTruthy();
  expect(within(menu).getByText('Requires Workflow tools')).toBeTruthy();
  await userEvent.click(within(menu).getByRole('menuitem', { name: /^Custom workflow/ }));
  const create = screen.getByRole('button', { name: 'Create track' });
  expect(create.hasAttribute('disabled') || create.getAttribute('aria-disabled') === 'true').toBe(true);
  await userEvent.keyboard('{Enter}');
  expect(onSubmit).not.toHaveBeenCalled();
  await userEvent.click(screen.getByRole('button', { name: 'Open plugin settings' }));
  expect(onOpenPluginSettings).toHaveBeenCalledOnce();
  expect(screen.getByLabelText('What this track should do').textContent).toBe('Fix the thing');
  view.rerender(<NewTrackForm {...props} templates={[{ ...TEMPLATE, availability: { status: 'ready' } }]} />);
  expect(create.hasAttribute('disabled') || create.getAttribute('aria-disabled') === 'true').toBe(false);
  await userEvent.click(create);
  expect(onSubmit).toHaveBeenCalledWith({ message: 'Fix the thing', template_id: TEMPLATE.id });
});

it('offers author-described working methods on arrival without opening the template picker', async () => {
  const onSubmit = vi.fn();
  render(<NewTrackForm submitting={false} error={null} templates={[TEMPLATE]} templatesLoaded
    initialTemplateId={null} initialCwd={null} onManageRecipes={vi.fn()} onOpenPluginSettings={vi.fn()}
    loadTemplate={(id) => Promise.resolve({ id, title: TEMPLATE.title, description: TEMPLATE.description, instructions: null, body: '# Report' })}
    listDirectory={vi.fn()} onSubmit={onSubmit} />);
  const choices = screen.getByRole('region', { name: 'Choose a working method' });
  expect(within(choices).getByText(TEMPLATE.description)).toBeTruthy();
  expect(within(choices).getByText('Requires Workflow tools')).toBeTruthy();
  await userEvent.click(within(choices).getByRole('button', { name: /^Custom workflow/ }));
  expect(screen.queryByRole('region', { name: 'Choose a working method' })).toBeNull();
  expect(screen.getByRole('button', { name: `Template: ${TEMPLATE.title}` })).toBeTruthy();
  expect(await screen.findByRole('region', { name: 'Selected template' })).toBeTruthy();
  expect(document.activeElement).toBe(screen.getByLabelText('What this track should do'));
  expect(onSubmit).not.toHaveBeenCalled();
});
