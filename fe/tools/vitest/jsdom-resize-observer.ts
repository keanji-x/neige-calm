// jsdom has no layout engine or ResizeObserver. DOM tests exercise component
// semantics; geometry and resize delivery are covered by the real browser tier.
// This setup file is never imported by production and leaves native observers intact.
if (typeof window !== 'undefined' && typeof globalThis.ResizeObserver === 'undefined') {
  globalThis.ResizeObserver = class implements ResizeObserver {
    observe(): void {}
    unobserve(): void {}
    disconnect(): void {}
  };
}

export {};
