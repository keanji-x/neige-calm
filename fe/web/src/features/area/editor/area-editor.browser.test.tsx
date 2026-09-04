import { cleanup, render, screen } from '@testing-library/react';
import { page } from 'vitest/browser';
import { afterEach, expect, it, vi } from 'vitest';

import '../../../styles/entry.css';
import { Dialog } from '../../../ui/dialog/public.tsx';
import { AreaEditorForm } from './public.tsx';

afterEach(cleanup);

it('renders both pills and both actions as one compact borderless row', async () => {
  await page.viewport(800, 600);
  render(
    <Dialog open title="New area" onClose={vi.fn()}>
      <AreaEditorForm
        initial={{ name: '', defaultTemplateId: null, defaultCwd: null }}
        submitting={false}
        error={null}
        templates={[]}
        templatesLoaded
        templatesError={null}
        listDirectory={vi.fn()}
        nameInputRef={{ current: null }}
        submitLabel="Create area"
        onCancel={vi.fn()}
        onSubmit={vi.fn()}
      />
    </Dialog>,
  );
  const template = screen.getByRole('button', { name: 'Default template: No template' });
  const folder = screen.getByRole('button', { name: 'Default folder: Neige workspace' });
  for (const pill of [template, folder]) {
    const style = getComputedStyle(pill);
    expect(style.borderStyle).toBe('none');
    expect(style.borderWidth).toBe('0px');
    expect(style.outlineStyle).toBe('none');
  }
  const templateBox = template.getBoundingClientRect();
  const folderBox = folder.getBoundingClientRect();
  const cancelBox = screen.getByRole('button', { name: 'Cancel' }).getBoundingClientRect();
  const createBox = screen.getByRole('button', { name: 'Create area' }).getBoundingClientRect();
  const centers = [templateBox, folderBox, cancelBox, createBox]
    .map((box) => box.top + box.height / 2);
  expect(Math.max(...centers) - Math.min(...centers)).toBeLessThan(1);
  expect(templateBox.height).toBe(folderBox.height);
});

it('wraps safely at phone width while keeping Cancel and Save together', async () => {
  await page.viewport(320, 640);
  render(
    <Dialog open title="Edit area" onClose={vi.fn()}>
      <AreaEditorForm
        initial={{
          name: 'Work',
          defaultTemplateId: 'long-template',
          defaultCwd: '/srv/a-folder-with-a-very-long-name',
        }}
        submitting={false}
        error={null}
        templates={[{
          id: 'long-template',
          title: 'A template name that is deliberately very long',
          tasks: [],
        }]}
        templatesLoaded
        templatesError={null}
        listDirectory={vi.fn()}
        nameInputRef={{ current: null }}
        submitLabel="Save changes"
        onCancel={vi.fn()}
        onSubmit={vi.fn()}
      />
    </Dialog>,
  );

  const dialog = screen.getByRole('dialog', { name: 'Edit area' });
  const cancelBox = screen.getByRole('button', { name: 'Cancel' }).getBoundingClientRect();
  const saveBox = screen.getByRole('button', { name: 'Save changes' }).getBoundingClientRect();
  const clear = screen.getByRole('button', { name: 'Use a new Neige workspace' });
  expect(dialog.scrollWidth).toBeLessThanOrEqual(dialog.clientWidth + 1);
  expect(Math.abs((cancelBox.top + cancelBox.height / 2) - (saveBox.top + saveBox.height / 2)))
    .toBeLessThan(1);
  expect(clear.getBoundingClientRect().right).toBeLessThanOrEqual(dialog.getBoundingClientRect().right);
});
