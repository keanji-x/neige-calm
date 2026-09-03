// @vitest-environment node
import { describe, expect, it } from 'vitest';

import type { CardEntry } from '../registry.js';
import { createCardRegistry } from '../registry.js';
import type { FileViewerCard } from './file-viewer.tsx';
import { FILE_VIEWER_CARD_ENTRY } from './file-viewer.tsx';
import { registerAvailableBuiltinCards } from './register.js';

const entry = FILE_VIEWER_CARD_ENTRY as CardEntry<FileViewerCard>;

describe('FILE_VIEWER_CARD_ENTRY', () => {
  it('resolves a kernel file-viewer row into the path it carries', () => {
    expect(entry.fromKernel?.({ id: 'f1', kind: 'file-viewer', payload: { path: '/repo/notes.md' } }))
      .toEqual({ type: 'file-viewer', id: 'f1', title: null, path: '/repo/notes.md' });
  });

  /*
   * A row with no usable `path` is not a degraded file card — there is nothing
   * for it to show. Refusing it puts the card in the board's `unknown` branch,
   * which draws a head with a delete on it; claiming it would draw a viewer
   * pointed at nothing, with no way out but deleting the track.
   */
  it('refuses a row whose payload carries no usable path', () => {
    for (const payload of [null, undefined, {}, { path: '' }, { path: 7 }, 'x', { path: null }]) {
      expect(
        entry.fromKernel?.({ id: 'f1', kind: 'file-viewer', payload }),
        `${JSON.stringify(payload)} must not resolve`,
      ).toBeNull();
    }
  });

  it('claims only its own kernel kind', () => {
    expect(entry.claim).toEqual({ mode: 'exact', kind: 'file-viewer' });
    expect(entry.fromKernel?.({ id: 'x', kind: 'terminal', payload: { path: '/repo' } })).toBeNull();
  });

  /*
   * `generic` is what routes this kind through `POST /api/tracks/:id/cards`
   * rather than an endpoint of its own — correct precisely because the card
   * owns no runtime for the kernel to spawn. `registerCard` also refuses a
   * generic entry without an exact claim, since the claim is what supplies the
   * `kind` the row is written with.
   */
  it('creates generically, and its payload is exactly the path', () => {
    expect(entry.create?.mode).toBe('generic');
    if (entry.create?.mode !== 'generic') throw new Error('unreachable');
    expect(entry.create.buildPayload({ title: 'Notes', path: '/repo/notes.md' }))
      .toEqual({ path: '/repo/notes.md' });
    // `title` is a column on the row, not a member of the payload: putting it in
    // both is how the two start disagreeing about what the card is called.
    expect(Object.keys(entry.create.buildPayload({ title: 'Notes', path: '/x' }) as object))
      .toEqual(['path']);
  });

  it('offers a required file picker in the add menu', () => {
    expect(entry.addPanel?.label).toBe('file');
    expect(entry.addPanel?.fields?.map((field) => [field.key, field.kind, field.required === true]))
      .toEqual([['title', 'text', false], ['path', 'file', true]]);
  });

  it('registers as a surface-owning built-in and resolves through the production registry', () => {
    const registry = createCardRegistry();
    registerAvailableBuiltinCards(registry);
    expect(registry.get('file-viewer')?.headless).toBe(false);
    expect(registry.resolve({ id: 'f1', kind: 'file-viewer', payload: { path: '/repo' } })?.type)
      .toBe('file-viewer');
  });
});
