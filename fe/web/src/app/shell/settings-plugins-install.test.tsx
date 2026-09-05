// @vitest-environment jsdom
//
// Settings › Plugins › add and remove, wired to the real thing (#1480).
//
// The panes' own tests drive them through their props, which cannot see the
// claims that exist only once the app is assembled:
//
//   1. **the install request that leaves the browser is the one the kernel
//      documents** — `source.kind = "mcp_http"` with the credential and its
//      placement as sibling fields. The pane can be shown to build a draft;
//      that the draft becomes *that body* is a fact about
//      `installConnectorOperation` and the transport, and it is read here off
//      the recorded request.
//   2. **a blank credential is an absent key, not an empty string.** The
//      kernel reads absent as "no credential" and refuses an empty one, so the
//      difference is the whole keyless-connector case.
//   3. **removing a plugin issues one DELETE and then re-reads the list**, so
//      the row disappears because the kernel says so and not because the UI
//      decided to hide it.
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider, createMemoryHistory } from '@tanstack/react-router';
import { cleanup, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { createAppRouter } from '../router/public.tsx';
import { bootTestCardRuntime } from '../router/test-card-runtime.ts';
import { ThemeProvider } from '../theme/public.tsx';

const unauthorized = createUnauthorizedChannel({ enqueue: (task) => task() });

beforeEach(() => {
  vi.stubGlobal('matchMedia', vi.fn(() => ({
    matches: false,
    addEventListener: vi.fn(),
    removeEventListener: vi.fn(),
  })));
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

const ROW = {
  id: 'todo',
  version: '0.1.0',
  enabled: false,
  state: 'disabled',
  manifest_name: 'Todo',
  has_config: false,
};

type Reply = (request: ApiRequest) => ApiTransportResponse;

function renderPlugins(reply: Reply) {
  const calls: ApiRequest[] = [];
  const transport: ApiTransportPort = {
    send(request): Promise<ApiTransportResponse> {
      calls.push(request);
      return Promise.resolve(reply(request));
    },
  };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const router = createAppRouter({
    transport, unauthorized, client, cards: bootTestCardRuntime(), onSignOut: () => undefined,
  });
  router.update({ history: createMemoryHistory({ initialEntries: ['/settings/plugins'] }) });
  render(
    <QueryClientProvider client={client}>
      <ThemeProvider storage={{ getItem: () => null, setItem: () => undefined }}>
        <RouterProvider router={router} />
      </ThemeProvider>
    </QueryClientProvider>,
  );
  return calls;
}

const ok = (body: unknown): ApiTransportResponse => ({ status: 200, statusText: 'OK', body });

async function type(label: string, value: string) {
  await userEvent.type(await screen.findByLabelText(label), value);
}

describe('Settings › Plugins › add, end to end', () => {
  it('posts the connector the operator described, credential and placement together', async () => {
    const calls = renderPlugins((request) => {
      if (request.path === '/api/plugins') return ok([]);
      if (request.path === '/api/settings') return ok({ settings: {} });
      if (request.path === '/api/plugins/install') {
        return { status: 201, statusText: 'Created', body: { id: 'com.example.zhibao', enabled: false } };
      }
      return ok([]);
    });

    await userEvent.click(await screen.findByText('Add a plugin'));
    await type('Name', 'Zhibao');
    await type('Id', 'com.example.zhibao');
    await type('Server URL', 'https://mcp.wisburg.com/mcp');
    await type('API key', 'sk-live-credential');
    await userEvent.click(screen.getByRole('button', { name: 'Add plugin' }));

    await waitFor(() => {
      expect(calls.some((call) => call.path === '/api/plugins/install')).toBe(true);
    });
    const install = calls.find((call) => call.path === '/api/plugins/install');
    expect(install?.method).toBe('POST');
    expect(install?.body).toEqual({
      source: {
        kind: 'mcp_http',
        id: 'com.example.zhibao',
        display_name: 'Zhibao',
        url: 'https://mcp.wisburg.com/mcp',
        api_key: 'sk-live-credential',
        api_key_in: 'bearer',
      },
    });
    // The form leaves once the kernel has accepted, back to the list it added to.
    await screen.findByText('Add a plugin');
  });

  it('omits the credential entirely for a server that needs none', async () => {
    const calls = renderPlugins((request) => {
      if (request.path === '/api/plugins') return ok([]);
      if (request.path === '/api/settings') return ok({ settings: {} });
      if (request.path === '/api/plugins/install') {
        return { status: 201, statusText: 'Created', body: { id: 'open', enabled: false } };
      }
      return ok([]);
    });

    await userEvent.click(await screen.findByText('Add a plugin'));
    await type('Name', 'Open server');
    await type('Id', 'com.example.open');
    await type('Server URL', 'https://open.example.com/mcp');
    await userEvent.click(screen.getByRole('button', { name: 'Add plugin' }));

    await waitFor(() => {
      expect(calls.some((call) => call.path === '/api/plugins/install')).toBe(true);
    });
    const source = (calls.find((call) => call.path === '/api/plugins/install')
      ?.body as { source: Record<string, unknown> }).source;
    expect('api_key' in source).toBe(false);
    expect('api_key_in' in source).toBe(false);
  });

  it('installs a server directory by path', async () => {
    const calls = renderPlugins((request) => {
      if (request.path === '/api/plugins') return ok([]);
      if (request.path === '/api/settings') return ok({ settings: {} });
      if (request.path === '/api/plugins/install') {
        return { status: 201, statusText: 'Created', body: { id: 'todo', enabled: false } };
      }
      return ok([]);
    });

    await userEvent.click(await screen.findByText('Add a plugin'));
    await userEvent.click(screen.getByRole('combobox', { name: 'Source' }));
    await userEvent.click(await screen.findByRole('option', { name: 'Server directory' }));
    await type('Directory path', '/srv/neige/plugins/todo');
    await userEvent.click(screen.getByRole('button', { name: 'Add plugin' }));

    await waitFor(() => {
      expect(calls.some((call) => call.path === '/api/plugins/install')).toBe(true);
    });
    expect(calls.find((call) => call.path === '/api/plugins/install')?.body).toEqual({
      source: { kind: 'local_path', path: '/srv/neige/plugins/todo' },
    });
  });

  /* The refusal the operator meets most: an id somebody already used. It has to
     arrive on the form, in the kernel's own words, with the form still holding
     what was typed. */
  it('shows the kernel’s refusal without leaving the form', async () => {
    renderPlugins((request) => {
      if (request.path === '/api/plugins') return ok([]);
      if (request.path === '/api/settings') return ok({ settings: {} });
      if (request.path === '/api/plugins/install') {
        return {
          status: 409,
          statusText: 'Conflict',
          /* The kernel's own `ErrorBody`: the sentence is `error`, the code is
             `code`. A fixture that nested them would be asserting against a
             shape no route produces. */
          body: { error: 'plugin `todo` already installed at version `0.1.0`', code: 'plugin_conflict' },
        };
      }
      return ok([]);
    });

    await userEvent.click(await screen.findByText('Add a plugin'));
    await type('Name', 'Todo');
    await type('Id', 'todo');
    await type('Server URL', 'https://mcp.example.com/mcp');
    await userEvent.click(screen.getByRole('button', { name: 'Add plugin' }));

    const alert = await screen.findByRole('alert');
    expect(alert.textContent).toContain('already installed');
    expect(screen.getByLabelText<HTMLInputElement>('Id').value).toBe('todo');
  });
});

describe('Settings › Plugins › remove, end to end', () => {
  it('deletes once the question is answered, and re-reads the list', async () => {
    let installed = [ROW];
    const calls = renderPlugins((request) => {
      if (request.path === '/api/plugins') return ok(installed);
      if (request.path === '/api/settings') return ok({ settings: {} });
      if (request.path === '/api/plugins/todo' && request.method === 'DELETE') {
        installed = [];
        return { status: 204, statusText: 'No Content', body: null };
      }
      return ok([]);
    });

    await userEvent.click(await screen.findByRole('button', { name: 'Remove Todo' }));
    expect(calls.some((call) => call.method === 'DELETE')).toBe(false);
    await userEvent.click(screen.getByRole('button', { name: 'Remove Todo' }));

    await screen.findByText('No plugins installed.');
    const deletes = calls.filter((call) => call.method === 'DELETE');
    expect(deletes.map((call) => call.path)).toEqual(['/api/plugins/todo']);
  });
});
