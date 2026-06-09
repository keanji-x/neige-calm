import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import type { ReactNode } from 'react';
import { WaveReportSidebar } from './wave-report-sidebar';

vi.mock('../../app/theme', () => ({
  useTheme: () => ({ resolved: 'light' }),
}));

vi.mock('./file-viewer-codemirror', () => ({
  CodePane: ({ path, text }: { path: string; text: string }) => (
    <pre data-testid="code-pane" data-path={path}>
      {text}
    </pre>
  ),
}));

type MockRoute = {
  status?: number;
  body: unknown;
};

function makeClient(): QueryClient {
  return new QueryClient({
    defaultOptions: {
      queries: { retry: false, gcTime: 0 },
      mutations: { retry: false },
    },
  });
}

function renderWithClient(ui: ReactNode) {
  const client = makeClient();
  return render(
    <QueryClientProvider client={client}>{ui}</QueryClientProvider>,
  );
}

function installFetch(routes: Record<string, MockRoute>) {
  const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
    const url = new URL(String(input), 'http://localhost');
    const logicalPath = url.searchParams.get('path');
    const key =
      logicalPath === null ? url.pathname : `${url.pathname}?path=${logicalPath}`;
    const route = routes[key];
    if (!route) {
      throw new Error(`unmocked fetch: ${key}`);
    }
    return new Response(JSON.stringify(route.body), {
      status: route.status ?? 200,
      headers: { 'content-type': 'application/json' },
    });
  });
  vi.stubGlobal('fetch', fetchMock);
  return fetchMock;
}

describe('WaveReportSidebar', () => {
  beforeEach(() => {
    vi.unstubAllGlobals();
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it('renders root entries, lazily expands cards, and shows JSON in CodePane', async () => {
    const cardId = 'card_abc123456789';
    installFetch({
      '/api/waves/wave_1/files/ls': {
        body: [
          { name: 'index.md', kind: 'file' },
          { name: 'report.md', kind: 'file' },
          { name: 'cards/', kind: 'dir', size: 1 },
        ],
      },
      '/api/waves/wave_1/files/ls?path=cards': {
        body: [
          { name: 'index.json', kind: 'file' },
          { name: `${cardId}/`, kind: 'dir' },
        ],
      },
      '/api/waves/wave_1/files/cat?path=cards/index.json': {
        body: {
          content: JSON.stringify([{ id: cardId, kind: 'codex' }]),
          content_type: 'application/json',
        },
      },
      [`/api/waves/wave_1/files/ls?path=cards/${cardId}`]: {
        body: [{ name: 'payload.json', kind: 'file' }],
      },
      [`/api/waves/wave_1/files/cat?path=cards/${cardId}/payload.json`]: {
        body: {
          content: '{\n  "terminal_id": "term_1"\n}',
          content_type: 'application/json',
        },
      },
    });

    renderWithClient(<WaveReportSidebar waveId="wave_1" />);

    expect(await screen.findByRole('button', { name: /index\.md/ })).toBeTruthy();
    expect(screen.getByRole('button', { name: /cards\// })).toBeTruthy();
    expect(screen.getByText('Select a file')).toBeTruthy();

    fireEvent.click(screen.getByRole('button', { name: /cards\// }));
    const cardDir = await screen.findByRole('button', {
      name: /codex card_abc/,
    });
    fireEvent.click(cardDir);

    const payload = await screen.findByRole('button', { name: /payload\.json/ });
    fireEvent.click(payload);

    const pane = await screen.findByTestId('code-pane');
    expect(pane).toHaveAttribute(
      'data-path',
      `cards/${cardId}/payload.json`,
    );
    expect(pane).toHaveTextContent('"terminal_id": "term_1"');
  });

  it('renders the empty-root state', async () => {
    installFetch({
      '/api/waves/wave_1/files/ls': { body: [] },
    });

    renderWithClient(<WaveReportSidebar waveId="wave_1" />);

    expect(await screen.findByText('No files')).toBeTruthy();
  });

  it('renders CalmApiError text inline for root list failures', async () => {
    installFetch({
      '/api/waves/wave_1/files/ls': {
        status: 500,
        body: { error: 'wave fs unavailable', code: 'internal' },
      },
    });

    renderWithClient(<WaveReportSidebar waveId="wave_1" />);

    expect(await screen.findByRole('alert')).toHaveTextContent(
      'wave fs unavailable',
    );
  });

  it('renders markdown files with ReactMarkdown', async () => {
    installFetch({
      '/api/waves/wave_1/files/ls': {
        body: [{ name: 'report.md', kind: 'file' }],
      },
      '/api/waves/wave_1/files/cat?path=report.md': {
        body: {
          content: '# Rendered Report\n\n- one\n',
          content_type: 'text/markdown',
        },
      },
    });

    renderWithClient(<WaveReportSidebar waveId="wave_1" />);

    fireEvent.click(await screen.findByRole('button', { name: /report\.md/ }));

    expect(
      await screen.findByRole('heading', { name: 'Rendered Report' }),
    ).toBeTruthy();
  });
});
