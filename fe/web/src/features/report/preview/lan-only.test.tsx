// @vitest-environment jsdom
// @vitest-environment-options {"url":"https://calm.example.com/next/tracks/w1"}
// Its own file because the page scheme is the jsdom URL's, fixed per file.
import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';

import { ReportPreviewBlock } from './public.tsx';

afterEach(cleanup);

describe('ReportPreviewBlock on an https page', () => {
  it('draws no frame and says the preview is LAN-only, with its port', () => {
    const { container } = render(<ReportPreviewBlock payload={{ key: 'fe', path: '/next/' }}
      resolve={() => ({ status: 'registered', preview: { key: 'fe', title: 'FE', port: 4050, live: true } })} />);
    expect(container.querySelector('iframe')).toBeNull();
    expect(screen.getByRole('note').textContent).toBe('预览仅 LAN 可用 · port 4050');
  });

  it('says it even before the key is registered', () => {
    const { container } = render(<ReportPreviewBlock payload={{ key: 'fe' }} resolve={() => ({ status: 'missing' })} />);
    expect(container.querySelector('iframe')).toBeNull();
    expect(screen.getByRole('note').textContent).toBe('预览仅 LAN 可用');
  });
});
