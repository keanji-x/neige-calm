// @vitest-environment node
import { renderToString } from 'react-dom/server';
import { describe, expect, it } from 'vitest';
import { ThemeProvider, useTheme } from './public.tsx';

function Probe() {
  const { mode, resolved } = useTheme();
  return <output>{mode}:{resolved}</output>;
}

describe('ThemeProvider server contract', () => {
  it('INV-APP-077 renders without window, matchMedia, or document and has a positive light fallback', () => {
    expect(typeof window).toBe('undefined');
    expect(typeof document).toBe('undefined');
    expect(renderToString(<ThemeProvider><Probe /></ThemeProvider>)).toMatch(/system.*light/u);
  });
});
