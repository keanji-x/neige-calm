// @vitest-environment jsdom
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, it } from 'vitest';
import { nativeViewPayloadSchema } from '../../../../../core/domain/report-view.ts';
import { NativeReportView } from './public.tsx';
import { linePaths } from '../../../ui/data-visualization/public.tsx';

afterEach(cleanup);
const fixture = JSON.parse(readFileSync(resolve(process.cwd(), '../test-data/native-view-v1.json'), 'utf8')) as { valid: unknown };
const payload = nativeViewPayloadSchema.parse(fixture.valid);

it('renders native components with no iframe or application script', () => {
  const { container } = render(<NativeReportView payload={payload} />);
  expect(container.querySelector('iframe')).toBeNull();
  expect(container.querySelector('script')).toBeNull();
  expect(screen.getByText('已用空间').parentElement?.textContent).toContain('120 GB');
  expect(screen.getByText('尚未取得样本')).toBeTruthy();
  expect(screen.getAllByRole('img')).toHaveLength(2);
  expect(container.textContent).not.toMatch(/对账|复盘|交易/);
});

it('opens evidence as text and keeps handling distinct from the finding', async () => {
  const { container } = render(<NativeReportView payload={payload} />);
  await userEvent.click(screen.getByRole('button', { name: '查看证据' }));
  expect(screen.getByText('待补证')).toBeTruthy();
  expect(screen.getByText('未知')).toBeTruthy();
  expect(screen.getByText('<script>alert(1)</script>')).toBeTruthy();
  expect(container.querySelector('script')).toBeNull();
});

it('opens the existing native wide dialog and restores the opener', async () => {
  render(<NativeReportView payload={payload} />);
  const button = screen.getByRole('button', { name: '展开 运营概览' });
  await userEvent.click(button);
  expect(screen.getByRole('dialog', { name: '运营概览' })).toBeTruthy();
  expect(document.querySelector('iframe')).toBeNull();
  await userEvent.keyboard('{Escape}');
  expect(screen.queryByRole('dialog')).toBeNull();
});

it('keeps missing points as gaps and does not discard a real zero', () => {
  expect(linePaths([{ x: 0, y: 1 }, { x: 1, y: null }, { x: 2, y: 0 }])).toEqual(['M0,1', 'M2,0']);
});
