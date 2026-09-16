import '../../styles/entry.css';
import { cleanup, render } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { page } from 'vitest/browser';
import { SettingsIndex } from './navigation.tsx';

afterEach(cleanup);

it('groups settings categories in one borderless rounded list with no row dividers', async () => {
  await page.viewport(390, 844);
  const onSelect = vi.fn();
  render(<SettingsIndex onSelectSection={onSelect} />);
  const navigation = await page.getByRole('navigation', { name: 'Settings categories' }).findElement();
  const group = navigation.firstElementChild!;
  expect(getComputedStyle(group).borderRadius).toBe('16px');
  expect(getComputedStyle(group).borderTopWidth).toBe('0px');
  expect(getComputedStyle(group).padding).toBe('4px');
  const rows = [...navigation.querySelectorAll('li')];
  expect(rows).toHaveLength(5);
  for (const row of rows) {
    expect(getComputedStyle(row).borderBottomWidth).toBe('0px');
    expect(getComputedStyle(row).borderRadius).toBe('12px');
  }
  await page.getByRole('button', { name: 'General', exact: true }).click();
  expect(onSelect).toHaveBeenCalledExactlyOnceWith('general');
});
