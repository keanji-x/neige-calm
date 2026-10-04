import { act, cleanup, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { page, userEvent } from 'vitest/browser';
import { QueryClient } from '@tanstack/react-query';
import '../../styles/entry.css';
import { EventStream, type EventStreamSink } from '../../systems/events/event-stream.ts';
import { EventBridge } from '../events/event-bridge.tsx';
import { LiveConnectionProvider, ConnectionStatusScope } from '../providers/connection-status.tsx';
import { createUiPreferences, UiPreferencesProvider } from '../providers/ui-preferences.tsx';
import { ThemeProvider } from '../theme/public.tsx';
import { Sidebar, type SidebarProps } from './sidebar.tsx';

// Browser coverage pins the actual paint, hover disclosure, and event-to-shell path.
afterEach(cleanup);
function sidebar(overrides: Partial<SidebarProps> = {}) {
  return <Sidebar areas={[]} tracksByArea={new Map()} tracks={[]} currentPath="/" onGo={vi.fn()}
    onRequestCreateArea={vi.fn()} onRequestEditArea={vi.fn()} onDeleteArea={vi.fn()} onNewTrack={vi.fn()}
    onSetPinned={vi.fn()} onDeleteTrack={vi.fn()} onOpenSettings={vi.fn()} onOpenPlugins={vi.fn()}
    onSignOut={vi.fn()} collapsed={false} onToggleCollapsed={vi.fn()} {...overrides} />;
}
function frame(children: React.ReactNode, width = 280) {
  return <ThemeProvider storage={{ getItem: () => null, setItem: () => undefined }}><UiPreferencesProvider preferences={createUiPreferences()}>
    <div style={{ width, height: 700 }}>{children}</div>
  </UiPreferencesProvider></ThemeProvider>;
}
it('paints live connection changes without restarting the stream and reveals details on hover', async () => {
  await page.viewport(1200, 800);
  let sink!: EventStreamSink;
  const start = vi.fn((_configuration, _url, next: EventStreamSink) => { sink = next; next.connectionState('connecting'); });
  const stop = vi.fn();
  const stream = EventStream.create('ws://test.invalid', { start, stop });
  const cursor = { read: () => null, write: vi.fn(), adopt: vi.fn(), clear: vi.fn() };
  const client = new QueryClient();
  const mounted = render(frame(<LiveConnectionProvider>
    <EventBridge client={client} stream={stream} cursor={cursor} syncEventVersion={3} dbInstanceId="test" />
    {sidebar()}
  </LiveConnectionProvider>));
  const connecting = screen.getByRole('button', { name: '连接状态：正在连接' });
  const dot = connecting.firstElementChild!;
  const red = getComputedStyle(dot).backgroundColor;
  act(() => sink.connectionState('connected'));
  const connected = screen.getByRole('button', { name: '连接状态：已连接' });
  expect(getComputedStyle(connected.firstElementChild!).backgroundColor).not.toBe(red);
  const today = screen.getByRole('button', { name: 'Go to Today' }).getBoundingClientRect();
  expect(connected.getBoundingClientRect().left).toBeGreaterThanOrEqual(today.right);
  await userEvent.hover(connected);
  await waitFor(() => expect(screen.getByRole('tooltip').textContent).toBe('已连接'));
  await page.screenshot({ path: '../../../../test-results/neige-connection-connected.png' });
  act(() => sink.connectionState('disconnected'));
  const disconnected = screen.getByRole('button', { name: '连接状态：连接已断开' });
  expect(getComputedStyle(disconnected.firstElementChild!).backgroundColor).toBe(red);
  await waitFor(() => expect(screen.getByRole('tooltip').textContent).toBe('连接已断开'));
  await userEvent.click(disconnected);
  expect(screen.queryByRole('button', { name: '立即重试' })).toBeNull();
  expect(start).toHaveBeenCalledTimes(1);
  mounted.unmount();
  expect(stop).toHaveBeenCalledTimes(1);
});

it('hides raw sidebar errors until disclosure, keeps them accessible when collapsed, and retries', async () => {
  await page.viewport(1200, 800);
  const retry = vi.fn();
  const retryConnection = vi.fn();
  const build = (collapsed: boolean) => frame(<ConnectionStatusScope value={{ connected: false,
    label: '连接已断开', detail: 'WebSocket connection lost', retry: retryConnection }}>
    {sidebar({ collapsed, readError: 'HTTP 503: lost', activityError: 'service unavailable', onRetryRead: retry })}
  </ConnectionStatusScope>, collapsed ? 44 : 280);
  const view = render(build(false));
  expect(screen.queryByRole('alert')).toBeNull();
  expect(screen.queryByRole('dialog', { name: '连接详情' })).toBeNull();
  const indicator = screen.getByRole('button', { name: '连接状态：连接异常' });
  await userEvent.hover(indicator);
  await waitFor(() => expect(screen.getByRole('tooltip').textContent).toContain('HTTP 503: lost'));
  await page.screenshot({ path: '../../../../test-results/neige-connection-offline.png' });
  await userEvent.click(indicator);
  expect(within(screen.getByRole('dialog', { name: '连接详情' })).getByText('HTTP 503: lost')).toBeTruthy();
  const errorLine = within(screen.getByRole('dialog', { name: '连接详情' })).getByText('HTTP 503: lost');
  await waitFor(() => {
    const rect = errorLine.getBoundingClientRect();
    expect(errorLine.contains(document.elementFromPoint(rect.left + rect.width / 2, rect.top + rect.height / 2))).toBe(true);
  });
  await page.screenshot({ path: '../../../../test-results/neige-connection-menu.png' });
  await userEvent.click(screen.getByRole('button', { name: '立即重试' }));
  expect(retry).toHaveBeenCalledTimes(1);
  expect(retryConnection).toHaveBeenCalledTimes(1);
  view.rerender(build(true));
  const collapsed = screen.getByRole('button', { name: '连接状态：连接异常' });
  const bounds = collapsed.getBoundingClientRect();
  expect(bounds.width).toBeGreaterThan(0);
  expect(bounds.left).toBeGreaterThanOrEqual(0);
  expect(bounds.right).toBeLessThanOrEqual(44);
  expect(screen.queryByRole('alert')).toBeNull();
  await userEvent.hover(collapsed);
  await waitFor(() => expect(screen.getByRole('tooltip').textContent).toContain('WebSocket connection lost'));
  await page.screenshot({ path: '../../../../test-results/neige-connection-collapsed.png' });
});
