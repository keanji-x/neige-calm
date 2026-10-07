import './entry.css';
import { cleanup, render } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { ReportDocument } from '../features/report/document/public.tsx';

afterEach(() => {
  cleanup();
  delete document.documentElement.dataset.theme;
  document.documentElement.style.removeProperty('font-size');
});

it.each(['light', 'dark'])('retains serif heading hierarchy and independent action states in %s', theme => {
  document.documentElement.dataset.theme = theme;
  const view = render(<>
    <ReportDocument report={{ summary: '', body: '# 主章节\n\n正文内容。\n\n## 次章节\n\n更多内容。', blocks: null }} empty={<></>} />
    <button data-nc-action="destructive">Delete</button>
    <button data-nc-action="secondary" disabled>Disabled</button>
  </>);
  const first = view.container.querySelector<HTMLElement>('[data-nc-report] h2')!;
  const second = view.container.querySelector<HTMLElement>('[data-nc-report] h3')!;
  const paragraph = view.container.querySelector<HTMLElement>('[data-nc-report] p')!;
  expect(getComputedStyle(first).fontSize).toBe('20px');
  expect(getComputedStyle(first).fontWeight).toBe('700');
  expect(getComputedStyle(second).fontSize).toBe('16px');
  expect(getComputedStyle(second).fontWeight).toBe('600');
  expect(getComputedStyle(second).fontFamily).toBe(getComputedStyle(paragraph).fontFamily);
  expect(getComputedStyle(paragraph).fontSize).toBe('16px');
  const disabled = view.getByRole('button', { name: 'Disabled' });
  const destructive = view.getByRole('button', { name: 'Delete' });
  expect(getComputedStyle(disabled).color).not.toBe(getComputedStyle(destructive).color);
  expect(getComputedStyle(disabled).color).not.toBe(getComputedStyle(paragraph).color);
});
