import { cleanup, render } from '@testing-library/react';
import { page, userEvent } from 'vitest/browser';
import { afterEach, expect, it, vi } from 'vitest';

import '../../../styles/entry.css';
import { mentionQueryOf, mentionSuggestionsOf, type MentionSearch } from '../../../../../core/domain/mentions.ts';
import { ChatComposer } from './public.tsx';
import { useMentionTrigger } from './mention-trigger.tsx';

afterEach(cleanup);

function Composer({ search, onSend }: { search: MentionSearch; onSend: (text: string) => void }) {
  const trigger = useMentionTrigger(search);
  return <div style={{ position: 'fixed', top: '400px', left: '32px', width: 'min(400px, calc(100vw - 64px))' }}>
    <ChatComposer mentionTrigger={trigger} onSend={onSend} />
  </div>;
}

it.each([1200])('shows readable type entrances and enters each filtered list by mouse at %ipx', async (width) => {
  await page.viewport(width, 900);
  const search = vi.fn<MentionSearch>((query) => Promise.resolve(mentionSuggestionsOf({
    tags: [{ label: 'release', track_count: 2, insert: '@`tag:release`' }],
    tracks: [{ label: 'Release notes', track_id: 't1', insert: '@`area/reports/Release notes.md`' }],
    blocks: [{ label: 'Checklist', block_id: 'b1', track_id: 't1', track_title: 'Release notes', insert: '@`area/reports/Release notes.md#b1`' }],
  }, mentionQueryOf(query))));
  render(<Composer search={search} onSend={vi.fn()} />);
  const field = page.getByRole('combobox', { name: 'Message' });
  await field.click();
  await userEvent.keyboard('@');
  await expect.element(page.getByRole('option', { name: /^Tags/ })).toBeVisible();
  expect(document.querySelectorAll('[role="option"]')).toHaveLength(10);
  const row = document.querySelector('[data-nc-mention-category="tag"]')!;
  const label = row.children[0].getBoundingClientRect();
  const description = row.children[1].getBoundingClientRect();
  expect(description.left).toBeGreaterThan(label.right);
  const example = document.querySelector('[data-nc-mention="tag"]')!;
  const titleStyle = getComputedStyle(row.children[0]);
  const exampleStyle = getComputedStyle(example.children[1]);
  const captionStyle = getComputedStyle(row.children[1]);
  expect(titleStyle.fontSize).toBe(captionStyle.fontSize);
  expect(parseFloat(exampleStyle.fontSize)).toBeGreaterThan(parseFloat(titleStyle.fontSize));
  expect(titleStyle.fontFamily).toBe(getComputedStyle(document.body).fontFamily);
  expect(exampleStyle.fontFamily).toBe(titleStyle.fontFamily);
  expect(getComputedStyle(row).fontSize).toBe(exampleStyle.fontSize);
  expect(parseFloat(exampleStyle.fontSize)).toBeGreaterThan(parseFloat(captionStyle.fontSize));
  expect(example.children[0].textContent).toBe('#');
  expect(example.children[1].textContent).toBe('release');
  expect(example.children[0].getBoundingClientRect().left).toBe(label.left);
  const more = page.getByRole('option', { name: 'More Tags' }).element();
  expect(more.querySelector('[aria-hidden="true"]')!.getBoundingClientRect().left).toBe(label.left);
  expect(exampleStyle.color).not.toBe(titleStyle.color);
  const headingOptionStyle = getComputedStyle(row.closest('[role="option"]')!);
  expect(parseFloat(headingOptionStyle.marginBlockStart)).toBe(4);
  expect(row.closest('[role="option"]')!.getBoundingClientRect().height).toBe(24);
  expect(example.closest('[role="option"]')!.getBoundingClientRect().height).toBe(24);
  expect(description.right).toBe(example.children[2].getBoundingClientRect().right);
  expect(row.children).toHaveLength(2);
  await page.screenshot({ path: `./__screenshots__/mention-categories-${width}.png` });

  for (const [category, prefixText, result] of [
    ['Tags', '#', '#release'], ['Tracks', '/', 'Release notes'], ['Blocks', '>', 'Checklist'],
  ]) {
    await page.getByRole('option', { name: `More ${category}` }).click();
    await expect.element(field).toHaveTextContent(`@${prefixText}`);
    await expect.element(page.getByRole('option', { name: new RegExp(result) })).toBeVisible();
    expect(document.querySelectorAll('[role="option"]')).toHaveLength(1);
    expect(search).toHaveBeenLastCalledWith(prefixText, expect.anything());
    await userEvent.keyboard('{Backspace}');
    await expect.element(page.getByRole('option', { name: /^Tags/ })).toBeVisible();
  }
  await page.getByRole('option', { name: /^Plugins/ }).click();
  await expect.element(field).toHaveTextContent('@+');
  await expect.element(page.getByText('No matches')).toBeVisible();
  expect(document.querySelectorAll('[role="option"]')).toHaveLength(0);
});
