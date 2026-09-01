// The `ListDirectory` port, end to end over a recording transport — the real
// operation, the real decoder, and the real `joinDirectoryPath` the picker
// itself uses. `core/domain/fs.test.ts` pins the mapping with a stand-in join
// because `core` may not import `ui`; this is the half that proves the wiring
// hands the owner's function in, and that the two agree about where a row is.

import { describe, expect, it } from 'vitest';

import type { ApiRequest, ApiTransportPort } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { createDirectoryLister } from './directory.ts';

const unauthorized = createUnauthorizedChannel({ enqueue: (task: () => void) => task() });

function harness(body: unknown, status = 200) {
  const sent: ApiRequest[] = [];
  const transport: ApiTransportPort = {
    send(request) {
      sent.push(request);
      return Promise.resolve({ status, statusText: status === 200 ? 'OK' : 'Bad Request', body });
    },
  };
  return { sent, listDirectory: createDirectoryLister(transport, unauthorized) };
}

describe('createDirectoryLister', () => {
  it('asks for the server default when no path is given', async () => {
    const { sent, listDirectory } = harness({ path: '/home/kenji', parent: '/home', entries: [] });
    await listDirectory();
    expect(sent).toHaveLength(1);
    expect(sent[0]).toMatchObject({ method: 'GET', path: '/api/fs/listdir', credentials: 'include' });
  });

  it('places every entry at an absolute path built by the browser\'s own join', async () => {
    const { sent, listDirectory } = harness({
      path: '/home/kenji',
      parent: '/home',
      entries: [{ name: 'code', is_dir: true }, { name: 'todo.txt', is_dir: false }],
    });
    const listing = await listDirectory('/home/kenji');
    expect(sent[0]?.path).toBe('/api/fs/listdir?path=%2Fhome%2Fkenji');
    expect(listing).toEqual({
      path: '/home/kenji',
      parent: '/home',
      entries: [
        { name: 'code', path: '/home/kenji/code', isDirectory: true },
        { name: 'todo.txt', path: '/home/kenji/todo.txt', isDirectory: false },
      ],
    });
  });

  /* The root is the case a naive `${parent}/${name}` gets wrong — it yields
     `//usr`, a path the next listdir would answer for a different directory on
     some platforms and which no `parent` chain leads back to. */
  it('joins at the filesystem root without doubling the separator', async () => {
    const { listDirectory } = harness({
      path: '/', parent: null, entries: [{ name: 'usr', is_dir: true }],
    });
    const listing = await listDirectory('/');
    expect(listing.parent).toBeNull();
    expect(listing.entries[0]?.path).toBe('/usr');
  });

  it('rejects with the API error the browser renders as its inline message', async () => {
    const { listDirectory } = harness({ error: 'path /nope is not a directory' }, 400);
    await expect(listDirectory('/nope')).rejects.toThrow('path /nope is not a directory');
  });
});
