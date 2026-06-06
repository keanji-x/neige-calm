import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { act, cleanup, render, screen, waitFor } from '@testing-library/react';
import { useRef, type ReactNode } from 'react';
import { useCardVisibilityFocus } from './useCardVisibilityFocus';

const originalIntersectionObserver = globalThis.IntersectionObserver;
const originalMutationObserver = globalThis.MutationObserver;

class FakeIntersectionObserver {
  static instances: FakeIntersectionObserver[] = [];

  private readonly callback: IntersectionObserverCallback;
  readonly options: IntersectionObserverInit | undefined;
  observed = new Set<Element>();
  observe = vi.fn((target: Element) => {
    this.observed.add(target);
  });
  unobserve = vi.fn((target: Element) => {
    this.observed.delete(target);
  });
  disconnect = vi.fn(() => {
    this.observed.clear();
  });
  takeRecords = vi.fn(() => []);

  constructor(callback: IntersectionObserverCallback, options?: IntersectionObserverInit) {
    this.callback = callback;
    this.options = options;
    FakeIntersectionObserver.instances.push(this);
  }

  fire(entries: Array<Partial<IntersectionObserverEntry> & { target: Element }>) {
    this.callback(
      entries as IntersectionObserverEntry[],
      this as unknown as IntersectionObserver,
    );
  }
}

class FakeMutationObserver {
  static instances: FakeMutationObserver[] = [];

  private readonly callback: MutationCallback;
  observe = vi.fn();
  disconnect = vi.fn();
  takeRecords = vi.fn(() => []);

  constructor(callback: MutationCallback) {
    this.callback = callback;
    FakeMutationObserver.instances.push(this);
  }

  fire(records: Array<{ addedNodes?: Node[]; removedNodes?: Node[] }>) {
    this.callback(
      records.map(
        ({ addedNodes = [], removedNodes = [] }) =>
          ({
            addedNodes: addedNodes as unknown as NodeList,
            removedNodes: removedNodes as unknown as NodeList,
          }) as MutationRecord,
      ),
      this as unknown as MutationObserver,
    );
  }
}

function Harness({ children }: { children: ReactNode }) {
  const scrollRootRef = useRef<HTMLDivElement | null>(null);
  useCardVisibilityFocus(scrollRootRef);
  return (
    <div ref={scrollRootRef} data-testid="scroll-root">
      {children}
    </div>
  );
}

beforeEach(() => {
  vi.clearAllMocks();
  cleanup();
  FakeIntersectionObserver.instances = [];
  FakeMutationObserver.instances = [];
  globalThis.IntersectionObserver =
    FakeIntersectionObserver as unknown as typeof IntersectionObserver;
  globalThis.MutationObserver =
    FakeMutationObserver as unknown as typeof MutationObserver;
});

afterEach(() => {
  if (originalIntersectionObserver) {
    globalThis.IntersectionObserver = originalIntersectionObserver;
  } else {
    const mutableGlobal = globalThis as {
      IntersectionObserver?: typeof IntersectionObserver;
    };
    delete mutableGlobal.IntersectionObserver;
  }

  if (originalMutationObserver) {
    globalThis.MutationObserver = originalMutationObserver;
  } else {
    const mutableGlobal = globalThis as {
      MutationObserver?: typeof MutationObserver;
    };
    delete mutableGlobal.MutationObserver;
  }
});

describe('useCardVisibilityFocus', () => {
  it('observes card visibility against the viewport with zero threshold', async () => {
    render(
      <Harness>
        <section data-card-id="card-a" />
      </Harness>,
    );

    await waitFor(() =>
      expect(FakeIntersectionObserver.instances[0]).toBeDefined(),
    );
    const observer = FakeIntersectionObserver.instances[0]!;
    expect(observer.options?.root).toBeNull();
    expect(observer.options?.threshold).toBe(0);
  });

  it('observes card shells added after mount and unobserves removed shells', async () => {
    render(
      <Harness>
        <section data-card-id="card-a" />
      </Harness>,
    );

    const observer = await waitFor(() => {
      const instance = FakeIntersectionObserver.instances[0];
      expect(instance?.observe).toHaveBeenCalledTimes(1);
      return instance!;
    });
    const mutationObserver = FakeMutationObserver.instances[0]!;
    const root = screen.getByTestId('scroll-root');
    const addedShell = document.createElement('section');
    addedShell.dataset.cardId = 'card-b';

    act(() => {
      root.append(addedShell);
      mutationObserver.fire([{ addedNodes: [addedShell] }]);
    });
    expect(observer.observe).toHaveBeenCalledWith(addedShell);
    expect(observer.observe).toHaveBeenCalledTimes(2);

    act(() => {
      addedShell.remove();
      mutationObserver.fire([{ removedNodes: [addedShell] }]);
    });
    expect(observer.unobserve).toHaveBeenCalledWith(addedShell);
  });

  it('disconnects observers and removes focus listeners on unmount', async () => {
    const { unmount } = render(
      <Harness>
        <section data-card-id="card-a" />
      </Harness>,
    );

    const observer = await waitFor(() => {
      const instance = FakeIntersectionObserver.instances[0];
      expect(instance?.observe).toHaveBeenCalledTimes(1);
      return instance!;
    });
    const mutationObserver = FakeMutationObserver.instances[0]!;
    const root = screen.getByTestId('scroll-root');
    const removeEventListenerSpy = vi.spyOn(root, 'removeEventListener');

    unmount();

    expect(mutationObserver.disconnect).toHaveBeenCalledTimes(1);
    expect(observer.disconnect).toHaveBeenCalledTimes(1);
    expect(removeEventListenerSpy).toHaveBeenCalledTimes(2);
    expect(removeEventListenerSpy).toHaveBeenCalledWith(
      'focusin',
      expect.any(Function),
    );
    expect(removeEventListenerSpy).toHaveBeenCalledWith(
      'focusout',
      expect.any(Function),
    );
  });
});
