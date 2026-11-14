// @vitest-environment jsdom

import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, it, vi } from 'vitest';
import type { Area } from '../../../../core/domain/area.ts';
import { AreaSelector } from './area-selector.tsx';

const area: Area = {
  id: 'live', name: 'Live area', color: 'blue', sort: 0, kind: 'user',
  defaultTemplateId: null, defaultCwd: null, createdAt: 0, updatedAt: 0,
};

afterEach(cleanup);

it('names the retained draft without adding it to live navigation choices', async () => {
  const select = vi.fn();
  render(<AreaSelector areas={[area]} activeArea={undefined}
    draftArea={{ id: 'draft', name: 'Draft area' }} onSelectArea={select} onCreateArea={vi.fn()} />);
  await userEvent.click(screen.getByRole('button', { name: 'Switch area, Draft area' }));
  expect(screen.queryByRole('menuitem', { name: 'Draft area' })).toBeNull();
  await userEvent.click(screen.getByRole('menuitem', { name: 'Live area' }));
  expect(select).toHaveBeenCalledWith('live');
});

it('prefers the live Area name over retained draft metadata', () => {
  render(<AreaSelector areas={[area]} activeArea={area}
    draftArea={{ id: area.id, name: 'Old name' }} onSelectArea={vi.fn()} onCreateArea={vi.fn()} />);
  expect(screen.getByRole('button', { name: 'Switch area, Live area' })).toBeTruthy();
});

it('keeps a system Area out of the label and navigation choices', async () => {
  const system: Area = { ...area, kind: 'system' };
  render(<AreaSelector areas={[system]} activeArea={system}
    onSelectArea={vi.fn()} onCreateArea={vi.fn()} />);
  await userEvent.click(screen.getByRole('button', { name: 'Switch area, Choose area' }));
  expect(screen.queryByRole('menuitem', { name: area.name })).toBeNull();
  expect(screen.getByRole('menuitem', { name: 'New area' })).toBeTruthy();
});
