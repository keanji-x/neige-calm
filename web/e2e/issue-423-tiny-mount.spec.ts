import { test, expect, type APIRequestContext, type Page } from '@playwright/test';
import { seedWaveViewMode } from './helpers/reset';

type ResizeCommitRecord = {
  terminalId: string;
  cols: number;
  rows: number;
  epoch: number;
};

test.setTimeout(60_000);

test('#423 collapsed Claude xterm mount does not commit tiny geometry', async ({
  page,
}) => {
  await installTinyMountHarness(page);

  const cove = await createCove(page.request, `E2E #423 ${Date.now()}`);
  const wave = await createWave(page.request, cove.id, `E2E #423 wave ${Date.now()}`);
  await createClaudeCard(page.request, wave.id);
  await seedWaveViewMode(page.request, wave.id, 'grid');

  await page.goto(`/calm/wave/${wave.id}?testMounts=1`, {
    waitUntil: 'domcontentloaded',
  });
  await expect(page.locator('.codex-card-pty').first()).toBeVisible({
    timeout: 15_000,
  });
  await expect(
    page.locator('.xterm-view[data-terminal-id="term_issue_423_e2e"]'),
  ).toBeVisible({ timeout: 15_000 });

  await page.waitForTimeout(5_000);
  expect(await resizeCommitRecords(page)).toEqual([]);

  await page.evaluate(() => {
    document.getElementById('__issue423_tiny_mount_style')?.remove();
  });

  await page.waitForFunction(
    () => {
      const w = window as unknown as {
        __issue423ResizeCommits?: ResizeCommitRecord[];
      };
      return (w.__issue423ResizeCommits ?? []).some(
        (r) =>
          r.terminalId === 'term_issue_423_e2e' &&
          r.cols >= 8 &&
          r.rows >= 4,
      );
    },
    null,
    { timeout: 15_000 },
  );

  const commits = await resizeCommitRecords(page);
  expect(
    commits.some((r) => r.rows < 4),
    `ResizeCommit frames must never carry rows < 4. Frames: ${JSON.stringify(commits)}`,
  ).toBe(false);
  expect(
    commits.some(
      (r) =>
        r.terminalId === 'term_issue_423_e2e' &&
        r.cols >= 8 &&
        r.rows >= 4,
    ),
  ).toBe(true);
});

async function installTinyMountHarness(page: Page): Promise<void> {
  await page.addInitScript(() => {
    type ResizeCommitRecord = {
      terminalId: string;
      cols: number;
      rows: number;
      epoch: number;
    };
    type Issue423Window = Window & {
      __issue423ResizeCommits?: ResizeCommitRecord[];
    };

    const installCollapseStyle = () => {
      if (document.getElementById('__issue423_tiny_mount_style')) return;
      const style = document.createElement('style');
      style.id = '__issue423_tiny_mount_style';
      style.textContent = `
        .codex-card-pty {
          flex: 0 0 8px !important;
          height: 8px !important;
          max-height: 8px !important;
          min-height: 0 !important;
          overflow: hidden !important;
        }
        .codex-card-pty .xterm-view,
        .codex-card-pty .xterm-container {
          height: 8px !important;
          min-height: 0 !important;
        }
      `;
      document.head.appendChild(style);
    };
    if (document.head) {
      installCollapseStyle();
    } else {
      const observer = new MutationObserver(() => {
        if (!document.head) return;
        observer.disconnect();
        installCollapseStyle();
      });
      observer.observe(document.documentElement ?? document, {
        childList: true,
        subtree: true,
      });
    }

    const w = window as Issue423Window;
    w.__issue423ResizeCommits = [];

    const NativeWebSocket = window.WebSocket;

    class FakeTerminalWebSocket {
      static readonly CONNECTING = NativeWebSocket.CONNECTING;
      static readonly OPEN = NativeWebSocket.OPEN;
      static readonly CLOSING = NativeWebSocket.CLOSING;
      static readonly CLOSED = NativeWebSocket.CLOSED;

      readonly url: string;
      readonly protocol = '';
      readonly extensions = '';
      binaryType: BinaryType = 'blob';
      readyState = NativeWebSocket.CONNECTING;
      bufferedAmount = 0;
      onopen: ((ev: Event) => void) | null = null;
      onmessage: ((ev: MessageEvent<string>) => void) | null = null;
      onclose: ((ev: CloseEvent) => void) | null = null;
      onerror: ((ev: Event) => void) | null = null;
      private clientId: string | null = null;
      private terminalId = 'unknown';
      private listeners = new Map<string, Set<EventListenerOrEventListenerObject>>();

      constructor(url: string | URL) {
        this.url = String(url);
        window.setTimeout(() => {
          this.readyState = NativeWebSocket.OPEN;
          this.dispatch('open', new Event('open'));
        }, 0);
      }

      send(data: string | ArrayBufferLike | Blob | ArrayBufferView): void {
        if (typeof data !== 'string') return;
        const parsed = JSON.parse(data) as unknown;
        if (
          typeof parsed === 'object' &&
          parsed !== null &&
          'ClientHello' in parsed
        ) {
          const hello = (
            parsed as {
              ClientHello: {
                client_id: string;
                terminal_id: string;
                desired_size: { cols: number; rows: number };
              };
            }
          ).ClientHello;
          this.clientId = hello.client_id;
          this.terminalId = hello.terminal_id;
          window.setTimeout(() => {
            this.emitMessage({
              ServerHello: {
                protocol_version: 4,
                terminal_id: hello.terminal_id,
                session_id: '11111111-1111-4111-8111-111111111111',
                client_role: 'Observer',
                owner_client_id: '22222222-2222-4222-8222-222222222222',
                pty_size: {
                  cols: 80,
                  rows: 24,
                  pixel_width: null,
                  pixel_height: null,
                },
                pty_seq_head: 0,
                pty_seq_tail: 0,
                render_rev: 1,
                snapshot: {
                  render_rev: 1,
                  pty_seq: 0,
                  cols: hello.desired_size.cols,
                  rows: hello.desired_size.rows,
                  encoding: 'Vt',
                  data: [],
                  scrollback: null,
                },
                history_gap: null,
                is_child_ready: false,
              },
            });
          }, 0);
          return;
        }
        if (parsed === 'OwnerClaim') {
          window.setTimeout(() => {
            this.emitMessage({
              OwnerChanged: { owner_client_id: this.clientId },
            });
          }, 0);
          return;
        }
        if (
          typeof parsed === 'object' &&
          parsed !== null &&
          'ResizeCommit' in parsed
        ) {
          const commit = (
            parsed as {
              ResizeCommit: { cols: number; rows: number; epoch: number };
            }
          ).ResizeCommit;
          w.__issue423ResizeCommits?.push({
            terminalId: this.terminalId,
            cols: commit.cols,
            rows: commit.rows,
            epoch: commit.epoch,
          });
          window.setTimeout(() => {
            this.emitMessage({
              ResizeApplied: {
                epoch: commit.epoch,
                cols: commit.cols,
                rows: commit.rows,
                pty_seq: 0,
                render_rev: 1,
              },
            });
          }, 0);
        }
      }

      close(): void {
        this.readyState = NativeWebSocket.CLOSED;
        this.dispatch('close', new CloseEvent('close'));
      }

      addEventListener(
        type: string,
        listener: EventListenerOrEventListenerObject,
      ): void {
        const listeners = this.listeners.get(type) ?? new Set();
        listeners.add(listener);
        this.listeners.set(type, listeners);
      }

      removeEventListener(
        type: string,
        listener: EventListenerOrEventListenerObject,
      ): void {
        this.listeners.get(type)?.delete(listener);
      }

      dispatchEvent(event: Event): boolean {
        this.dispatch(event.type, event);
        return true;
      }

      private emitMessage(payload: unknown): void {
        this.dispatch(
          'message',
          new MessageEvent('message', { data: JSON.stringify(payload) }),
        );
      }

      private dispatch(type: string, event: Event): void {
        if (type === 'open') this.onopen?.(event);
        if (type === 'message') {
          this.onmessage?.(event as MessageEvent<string>);
        }
        if (type === 'close') this.onclose?.(event as CloseEvent);
        if (type === 'error') this.onerror?.(event);
        for (const listener of this.listeners.get(type) ?? []) {
          if (typeof listener === 'function') {
            listener.call(this, event);
          } else {
            listener.handleEvent(event);
          }
        }
      }
    }

    const WrappedWebSocket = function (
      this: WebSocket,
      url: string | URL,
      protocols?: string | string[],
    ) {
      if (String(url).includes('/api/terminals/')) {
        return new FakeTerminalWebSocket(url) as unknown as WebSocket;
      }
      return protocols === undefined
        ? new NativeWebSocket(url)
        : new NativeWebSocket(url, protocols);
    } as unknown as typeof WebSocket;
    WrappedWebSocket.CONNECTING = NativeWebSocket.CONNECTING;
    WrappedWebSocket.OPEN = NativeWebSocket.OPEN;
    WrappedWebSocket.CLOSING = NativeWebSocket.CLOSING;
    WrappedWebSocket.CLOSED = NativeWebSocket.CLOSED;
    window.WebSocket = WrappedWebSocket;
  });
}

async function resizeCommitRecords(page: Page): Promise<ResizeCommitRecord[]> {
  return page.evaluate(() => {
    const w = window as unknown as {
      __issue423ResizeCommits?: ResizeCommitRecord[];
    };
    return w.__issue423ResizeCommits ?? [];
  });
}

async function createCove(
  request: APIRequestContext,
  name: string,
): Promise<{ id: string }> {
  const response = await request.post('/api/coves', {
    data: { name, color: '#6a8' },
    headers: { 'content-type': 'application/json' },
  });
  if (!response.ok()) {
    throw new Error(await formatApiError('create cove', response));
  }
  return (await response.json()) as { id: string };
}

async function createWave(
  request: APIRequestContext,
  coveId: string,
  title: string,
): Promise<{ id: string }> {
  const response = await request.post('/api/waves', {
    data: {
      cove_id: coveId,
      title,
      cwd: `/tmp/playwright-cove-${coveId}`,
      attach_folder: true,
      theme: { fg: [216, 219, 226], bg: [15, 20, 24] },
    },
    headers: { 'content-type': 'application/json' },
  });
  if (!response.ok()) {
    throw new Error(await formatApiError('create wave', response));
  }
  return (await response.json()) as { id: string };
}

async function createClaudeCard(
  request: APIRequestContext,
  waveId: string,
): Promise<void> {
  const response = await request.post(`/api/waves/${waveId}/cards`, {
    data: {
      kind: 'claude',
      payload: {
        schemaVersion: 1,
        terminal_id: 'term_issue_423_e2e',
        cwd: '/tmp',
      },
    },
    headers: { 'content-type': 'application/json' },
  });
  if (!response.ok()) {
    throw new Error(await formatApiError('create Claude card', response));
  }
}

async function formatApiError(label: string, response: {
  status(): number;
  statusText(): string;
  text(): Promise<string>;
}): Promise<string> {
  const body = await response.text().catch(() => '<unreadable>');
  return `${label}: ${response.status()} ${response.statusText()}: ${body}`;
}
