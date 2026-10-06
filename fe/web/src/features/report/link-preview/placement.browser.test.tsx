import { cleanup, render } from '@testing-library/react';
import { page } from 'vitest/browser';
import { afterEach, describe, expect, it } from 'vitest';

import '../../../styles/entry.css';
import { ReportDocument } from '../document/public.tsx';

afterEach(cleanup);

describe('report preview placement through the production renderer', () => {
  it('places growing report previews beside the reading column without covering the link', async () => {
    await page.viewport(1200, 800);
    const report = { summary: '', body: '[Notes](./notes.md)', blocks: null };
    render(<div style={{ position: 'absolute', left: 100, top: 640, width: 480, ['--document-start' as string]: '0px', ['--document-measure' as string]: '480px' }}>
      <ReportDocument report={report} empty={null} fileRoot="/repo" linkPreview={{ trackId: 't1', report, files: {
        readFile: (path) => Promise.resolve({ path, text: '# Long contents\n\n' + 'Reading content.\n\n'.repeat(60), size: 1000, truncated: false }),
        rawUrl: (path) => path,
      } }} />
    </div>);
    await page.getByRole('button', { name: 'Notes', exact: true }).hover();
    await expect.poll(() => page.getByRole('heading', { name: 'Long contents' }).query()).not.toBeNull();
    const link = page.getByRole('button', { name: 'Notes', exact: true }).element().getBoundingClientRect();
    const card = page.getByRole('dialog').element().getBoundingClientRect();
    expect(card.left).toBeGreaterThanOrEqual(592);
    expect(card.right).toBeLessThanOrEqual(1188);
    expect(card.left >= link.right || card.right <= link.left || card.top >= link.bottom || card.bottom <= link.top).toBe(true);
  });
});
