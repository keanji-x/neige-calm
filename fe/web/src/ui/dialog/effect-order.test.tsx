import { beforeEach, describe, expect, it, vi } from 'vitest';

const harness = vi.hoisted(() => ({ effects: [] as Array<() => void | (() => void)> }));
vi.mock('react', async (original) => ({
  ...await original<typeof import('react')>(),
  createContext: () => ({ Provider: ({ children }: { children: unknown }) => children }),
  useContext: () => null,
  useEffect: (effect: () => void | (() => void)) => { harness.effects.push(effect); },
  useMemo: <T,>(factory: () => T) => factory(),
  useRef: <T,>(value: T) => ({ current: value }),
}));
vi.mock('../state/public.ts', () => ({ useState: <T,>(value: T) => [value, vi.fn()] as const }));
vi.mock('react-dom', () => ({ createPortal: (node: unknown) => node }));

import { Dialog } from './public.tsx';

class FakeElement {
  parentElement: FakeElement | null = null;
  attributes = new Map<string, string>();
  focus = vi.fn(() => { if (!this.hasAttribute('inert')) fakeDocument.activeElement = this; });
  hasAttribute(name: string) { return this.attributes.has(name); }
  getAttribute(name: string) { return this.attributes.get(name) ?? null; }
  setAttribute(name: string, value: string) { this.attributes.set(name, value); }
  removeAttribute(name: string) { this.attributes.delete(name); }
  closest() { return null; }
  querySelectorAll() { return []; }
  contains(element: unknown) { return element === this; }
}
const target = new FakeElement();
const fakeDocument = {
  body: Object.assign(new FakeElement(), { style: { overflow: '' }, children: [target] }),
  activeElement: target,
  addEventListener: vi.fn(), removeEventListener: vi.fn(), contains: (element: unknown) => element === target,
};

describe('Dialog cleanup order regression', () => {
  beforeEach(() => { harness.effects.length = 0; target.attributes.clear(); target.focus.mockClear(); fakeDocument.activeElement = target; });
  it('removes inert before restoring focus to the background trigger', () => {
    vi.stubGlobal('HTMLElement', FakeElement);
    vi.stubGlobal('document', fakeDocument);
    vi.stubGlobal('requestAnimationFrame', vi.fn(() => 1));
    vi.stubGlobal('cancelAnimationFrame', vi.fn());
    Dialog({ open: true, title: 'Test', onClose: vi.fn() });
    const cleanups = harness.effects.map((effect) => effect()).filter((cleanup): cleanup is () => void => typeof cleanup === 'function');
    expect(target.hasAttribute('inert')).toBe(true);
    for (const cleanup of cleanups) cleanup();
    expect(target.hasAttribute('inert')).toBe(false);
    expect(target.focus).toHaveBeenCalledOnce();
    expect(fakeDocument.activeElement).toBe(target);
  });
});
