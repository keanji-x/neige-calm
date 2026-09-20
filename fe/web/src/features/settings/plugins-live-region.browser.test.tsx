// The empty boundary live region costs a plugin row nothing.
// Pure layout: `.pluginEffectBoundary:empty { position: absolute }` takes it out of the flex flow, and jsdom computes no layout.
import { render } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { page } from 'vitest/browser';

import '../../styles/entry.css';

import { PluginsPane } from './plugins.tsx';

afterEach(() => { document.body.replaceChildren(); });

it('costs a plugin row no height while it is empty', async () => {
  await page.viewport(1180, 720);
  const { container } = render(
    <PluginsPane
      plugins={[{
        id: 'todo',
        version: '0.1.0',
        enabled: true,
        state: 'running',
        manifest_name: 'Todo',
        manifest_description: 'Tracks what is left to do.',
        has_config: false,
      }]}
      loadError={null}
      onRetryLoad={vi.fn()}
      pendingIds={new Set()}
      errors={new Map()}
      effectBoundaryIds={new Set()}
      onSetEnabled={vi.fn()}
      onOpenConfig={vi.fn()}
      onAdd={vi.fn()}
      onUninstall={vi.fn()}
    />,
  );
  await new Promise<void>((resolve) => {
    requestAnimationFrame(() => requestAnimationFrame(() => { resolve(); }));
  });

  /* Located by the row's own attribute: astryx puts a `role="status"` region inside every Button. */
  const region = container.querySelector('[data-nc-effect-boundary]');
  expect(region).not.toBeNull();
  expect(region?.textContent).toBe('');
  const meta = region?.parentElement;
  expect(meta).not.toBeNull();
  if (region === null || meta === null || meta === undefined) return;

  const heightOf = () => meta.getBoundingClientRect().height;
  const gap = Number.parseFloat(getComputedStyle(meta).rowGap);
  expect(gap).toBeGreaterThan(0);

  const asShipped = heightOf();

  // Control 1 — the region put back into the flow, as the pane looks without the `:empty` rule.
  (region as HTMLElement).style.position = 'static';
  const inFlow = heightOf();
  (region as HTMLElement).style.removeProperty('position');
  expect(heightOf()).toBe(asShipped);

  // Control 2 — the region gone entirely: the height a row would have if this
  // feature had never been added.
  region.remove();
  const withoutRegion = heightOf();

  expect(asShipped).toBe(withoutRegion);
  /* In flow the same region costs exactly one `row-gap`, which proves the measurement can see a gap at all. */
  expect(inFlow - asShipped).toBeCloseTo(gap, 1);
});
