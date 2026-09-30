import { act, cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, it, vi } from 'vitest';
import type { TemplateDetail } from '../../../../../core/domain/template.ts';
import { NewTrackForm } from './public.tsx';
import { TemplatePreview } from './template-preview.tsx';

afterEach(cleanup);
function detail(id: string): TemplateDetail {
  return { id, title: id, description: `Purpose of ${id}`, instructions: 'Inspect the change.\nVerify the result.', body: '<script>window.previewExecuted = true</script>' };
}

it('shows the author-owned working method beneath the actual composer', async () => {
  render(<NewTrackForm submitting={false} error={null} templates={[{ id: 'small-change', title: 'Small change', tasks: [] }]}
    templatesLoaded initialTemplateId="small-change" initialCwd={null} onManageRecipes={vi.fn()}
    loadTemplate={(id) => Promise.resolve(detail(id))} listDirectory={vi.fn()} onSubmit={vi.fn()} />);
  expect(await screen.findByText('Purpose of small-change')).toBeTruthy();
  const preview = screen.getByRole('region', { name: 'Selected template' });
  expect(preview.compareDocumentPosition(screen.getByLabelText('What this track should do')) & Node.DOCUMENT_POSITION_PRECEDING).toBeTruthy();
  expect(screen.getByText('Working method')).toBeTruthy();
  await userEvent.click(screen.getByText('View full template'));
  expect(screen.getByText('<script>window.previewExecuted = true</script>')).toBeTruthy();
  expect(preview.querySelector('script')).toBeNull();
  await userEvent.click(screen.getByRole('button', { name: 'Template: Small change' }));
  await userEvent.click(screen.getByRole('menuitem', { name: 'No template' }));
  expect(screen.queryByRole('region', { name: 'Selected template' })).toBeNull();
});

it('never lets a late detail response replace the newly selected template', async () => {
  let resolveFirst!: (value: TemplateDetail) => void;
  const first = new Promise<TemplateDetail>((resolve) => { resolveFirst = resolve; });
  const loadTemplate = vi.fn((id: string) => id === 'first' ? first : Promise.resolve(detail(id)));
  const props = { loadTemplate };
  const view = render(<TemplatePreview {...props} id="first" title="First" />);
  view.rerender(<TemplatePreview {...props} id="second" title="Second" />);
  expect(await screen.findByText('Purpose of second')).toBeTruthy();
  await act(async () => { resolveFirst(detail('first')); await first; });
  expect(screen.queryByText('Purpose of first')).toBeNull();
  expect(screen.getByText('Purpose of second')).toBeTruthy();
});

it('offers a retry after failure and refuses details for a different id', async () => {
  const loadTemplate = vi.fn().mockResolvedValueOnce(detail('wrong')).mockResolvedValueOnce(detail('chosen'));
  render(<TemplatePreview id="chosen" title="Chosen" loadTemplate={loadTemplate} />);
  await userEvent.click(await screen.findByRole('button', { name: 'Retry preview' }));
  expect(await screen.findByText('Purpose of chosen')).toBeTruthy();
  expect(screen.queryByText('Purpose of wrong')).toBeNull();
});

it('previews a recipe locally without loading a same-named template', async () => {
  const loadTemplate = vi.fn();
  render(<TemplatePreview id="small-change" title="My recipe" recipeBody="# My own content"
    loadTemplate={loadTemplate} />);
  await userEvent.click(screen.getByText('View recipe content'));
  expect(screen.getByText('# My own content')).toBeTruthy();
  expect(loadTemplate).not.toHaveBeenCalled();
});
