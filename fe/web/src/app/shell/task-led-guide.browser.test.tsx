import '../../styles/entry.css';
import { cleanup, render } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { page } from 'vitest/browser';
import type { ReactNode } from 'react';
import { GuideProvider, parseGuideCatalog } from '../../ui/floating-control/public.ts';
import { useState } from '../../ui/state/public.ts';
import { NewTrackForm } from '../../features/area/new-track/public.tsx';
import { ChatComposer } from '../../features/chat/thread/public.tsx';
import { createUiPreferences } from '../providers/ui-preferences.tsx';
import { renderDailyFixture } from '../router/daily-planner-fixture.tsx';
import definition from './workspace-guides.json';

afterEach(cleanup);

function Guided({ children }: { children: ReactNode }) {
  const [catalog] = useState(() => parseGuideCatalog(definition));
  const [seen, remember] = useState(() => new Set<string>());
  const [enabled, enable] = useState(true);
  return <GuideProvider catalog={catalog} enabled={enabled} sessionKey="task" facts={{ 'has-projects': true, 'has-work': true }}
    isSeen={id => seen.has(id)} onSeen={id => remember(current => new Set([...current, id]))} onSkip={() => enable(false)}>
    {children}
  </GuideProvider>;
}

for (const width of [390, 1440]) {
  it(`fills the real new-track draft without sending and preserves it on Skip at ${width}px`, async () => {
    await page.viewport(width, 900);
    const onSubmit = vi.fn();
    render(<Guided><NewTrackForm submitting={false} error={null} templates={[]} templatesLoaded initialTemplateId={null}
      initialCwd={null} onManageRecipes={vi.fn()} listDirectory={vi.fn()} loadTemplate={vi.fn()} onSubmit={onSubmit} /></Guided>);
    await expect.element(page.getByRole('note', { name: '让 AI 做出第一个可见成果' })).toBeVisible();
    await expect.element(page.getByRole('button', { name: '下一步', exact: true })).toBeDisabled();
    const example = page.getByRole('button', { name: '填入练习需求', exact: true });
    const bounds = example.element().getBoundingClientRect();
    expect(bounds.left).toBeGreaterThanOrEqual(0);
    expect(bounds.right).toBeLessThanOrEqual(width);
    await example.click();
    const input = page.getByLabelText('What this track should do', { exact: true });
    await expect.element(input).toHaveTextContent('index.html');
    await expect.element(input).toHaveTextContent('不覆盖或删除已有文件');
    expect(document.activeElement).toBe(input.element());
    expect(onSubmit).not.toHaveBeenCalled();
    await expect.element(example).not.toBeInTheDocument();
    await page.getByRole('button', { name: '跳过引导', exact: true }).click();
    await expect.element(input).toHaveTextContent('index.html');
    expect(onSubmit).not.toHaveBeenCalled();
  });
}

it('does not offer to overwrite an existing new-track draft', async () => {
  render(<Guided><NewTrackForm submitting={false} error={null} templates={[]} templatesLoaded initialTemplateId={null}
    initialCwd={null} initialDraft={{ message: 'My existing request', selected: { kind: 'none' }, templateValues: {}, cwd: '' }}
    onManageRecipes={vi.fn()} listDirectory={vi.fn()} loadTemplate={vi.fn()} onSubmit={vi.fn()} /></Guided>);
  await expect.element(page.getByRole('note', { name: '让 AI 做出第一个可见成果' })).toBeVisible();
  await expect.element(page.getByRole('button', { name: '填入练习需求' })).not.toBeInTheDocument();
  await expect.element(page.getByLabelText('What this track should do', { exact: true })).toHaveTextContent('My existing request');
});

it('fills a real follow-up composer without sending and never offers it during a running turn', async () => {
  const onSend = vi.fn();
  const view = render(<Guided><ChatComposer onSend={onSend} /></Guided>);
  await page.getByRole('button', { name: '填入一次修改需求' }).click();
  await expect.element(page.getByLabelText('Message', { exact: true })).toHaveTextContent('我的第一个作品');
  expect(onSend).not.toHaveBeenCalled();
  view.unmount();
  render(<Guided><ChatComposer onSend={onSend} onStop={vi.fn()} /></Guided>);
  await expect.element(page.getByRole('note', { name: '在这里提出修改或回答问题' })).toBeVisible();
  await expect.element(page.getByRole('button', { name: '填入一次修改需求' })).not.toBeInTheDocument();
});

it('fills only the new project name and waits for the native Create action', async () => {
  await page.viewport(1280, 900);
  const preferences = createUiPreferences(); preferences.setDatabaseScope('task-name');
  const fixture = renderDailyFixture({ emptyWorkspace: true, uiPreferences: preferences });
  await page.getByRole('button', { name: 'New area', exact: true }).click();
  await expect.element(page.getByRole('button', { name: '下一步', exact: true })).toBeDisabled();
  await page.getByRole('button', { name: '填入这个名字', exact: true }).click();
  await expect.element(page.getByRole('textbox', { name: 'Name', exact: true })).toHaveValue('我的练习项目');
  expect(fixture.requests.filter(request => request.method === 'POST' && request.path === '/api/areas')).toHaveLength(0);
  await page.getByRole('button', { name: '下一步', exact: true }).click();
  await expect.element(page.getByRole('note', { name: '这次先做一个本地小练习' })).toBeVisible();
  fixture.client.clear();
});
