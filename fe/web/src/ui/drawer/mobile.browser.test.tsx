import { render } from '@testing-library/react';
import { page } from 'vitest/browser';
import { afterEach, describe, expect, it, vi } from 'vitest';

import '../../styles/entry.css';

import { Drawer } from './public.tsx';

afterEach(() => { document.body.replaceChildren(); });

describe('Drawer mobile Header', () => {
  it('opens a new Chat as Untitled with the shared Back-first Header', async () => {
    await page.viewport(390, 844);
    const onClose = vi.fn();
    render(
      <main style={{ position: 'relative', blockSize: '100dvh' }}>
        <Drawer
          open
          title="Untitled"
          mobileBackLabel="Report"
          onClose={onClose}
          footer={<form aria-label="Chat composer"><textarea aria-label="Message" /></form>}
        >
          <p>Start a new conversation about this Report.</p>
        </Drawer>
      </main>,
    );

    expect(page.getByRole('heading', { name: 'Untitled' })).toBeTruthy();
    expect(page.getByRole('button', { name: 'Back to Report' })).toBeTruthy();
    expect(document.querySelector('[data-nc-mobile-header]')).not.toBeNull();
    expect(document.querySelector('button[aria-label="Close conversation"]')).toBeNull();
    await new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve())));
    await page.screenshot({ path: '../../../../test-results/mobile-chat.png' });
  });
});
