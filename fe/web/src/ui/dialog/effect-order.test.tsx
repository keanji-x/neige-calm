import { cleanup, render } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { Dialog } from './public.tsx';

// eslint-disable-next-line @typescript-eslint/unbound-method -- the test restores and invokes this DOM prototype method with an explicit receiver.
const nativeFocus = HTMLElement.prototype.focus;

describe('Dialog cleanup order regression', () => {
  beforeEach(() => {
    vi.stubGlobal('requestAnimationFrame', (callback: FrameRequestCallback) => { callback(0); return 1; });
    vi.stubGlobal('cancelAnimationFrame', vi.fn());
    HTMLElement.prototype.focus = function focus(options?: FocusOptions) {
      if (!this.closest('[inert]')) nativeFocus.call(this, options);
    };
  });
  afterEach(() => { cleanup(); HTMLElement.prototype.focus = nativeFocus; vi.unstubAllGlobals(); });

  it('removes inert before restoring focus to the real background trigger', () => {
    const trigger = document.body.appendChild(document.createElement('button'));
    trigger.textContent = 'Open';
    trigger.focus();
    const result = render(<Dialog open title="Test" onClose={vi.fn()}/>);
    expect(trigger.closest('[inert]')).not.toBeNull();
    result.unmount();
    expect(document.activeElement).toBe(trigger);
    trigger.remove();
  });
});
