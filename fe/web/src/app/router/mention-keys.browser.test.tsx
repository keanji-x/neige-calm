import { cleanup, render } from '@testing-library/react';
import { page, userEvent } from 'vitest/browser';
import { afterEach, expect, it, vi } from 'vitest';

import '../../styles/entry.css';
import type { ApiRequest, ApiTransportPort } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import type { MentionSearch } from '../../../../core/domain/mentions.ts';
import { ChatComposer } from '../../features/chat/thread/public.tsx';
import { useMentionTrigger } from '../../features/chat/thread/mention-trigger.tsx';
import { mentionSearchOf } from '../providers/mentions.ts';

afterEach(cleanup);

function PluginComposer({ search, onSend }: { search: MentionSearch; onSend: (text: string) => void }) {
  const trigger = useMentionTrigger(search);
  return <div style={{ position: 'fixed', top: '400px', left: '32px', width: '400px' }}>
    <ChatComposer mentionTrigger={trigger} onSend={onSend} />
  </div>;
}

it('opens real plugin guides from their preview and sends a named documentation reference', async () => {
  await page.viewport(1200, 900);
  const requests: ApiRequest[] = [];
  const transport: ApiTransportPort = { send(request) {
    requests.push(request);
    return Promise.resolve({ status: 200, statusText: 'OK', body: request.path === '/api/plugins' ? [
      { id: 'dev.example', version: '1', enabled: false, state: 'disabled', manifest_name: 'Development',
        manifest_description: 'Develop issues and publish changes. '.repeat(20), has_config: false, can_uninstall: false, can_disable: true },
      { id: 'ui.example', version: '1', enabled: false, state: 'disabled', manifest_name: 'UI tools',
        manifest_description: 'Review interface changes.', has_config: false, can_uninstall: false, can_disable: true },
    ] : { tags: [{ label: 'release', track_count: 2, insert: '@`tag:release`' }], tracks: [], blocks: [] } });
  } };
  const search = mentionSearchOf(transport, createUnauthorizedChannel({ enqueue: task => task() }), 'area', 'track');
  const onSend = vi.fn();
  render(<PluginComposer search={search} onSend={onSend} />);
  const field = page.getByRole('combobox', { name: 'Message' });
  await field.click();
  await userEvent.keyboard('see @');
  await expect.element(page.getByRole('option', { name: /Development/ })).toBeVisible();
  const pluginRow = document.querySelector('[data-nc-mention="plugin"]')!;
  expect(pluginRow.children[0].textContent).toBe('+');
  await page.getByRole('option', { name: 'More Plugins' }).click();
  await expect.element(field).toHaveTextContent('see @+');
  await expect.element(page.getByRole('option', { name: /Development/ })).toBeVisible();
  expect(requests.map(request => [request.method, request.path])).toEqual([
    ['GET', '/api/areas/area/mentions?q=&track=track'], ['GET', '/api/plugins'], ['GET', '/api/plugins'],
  ]);
  expect(document.querySelectorAll('[role="option"]')).toHaveLength(2);
  await page.getByRole('option', { name: /Development/ }).click();
  expect(field.element().querySelector('[data-astryx-token]')?.textContent).toBe('Development');
  await userEvent.keyboard('{Enter}');
  expect(onSend).toHaveBeenCalledOnce();
  expect(onSend.mock.calls[0][0]).toContain('see Plugin reference (documentation only; kernel permissions still apply):');
  expect(onSend.mock.calls[0][0]).toContain('dev.example');
});
