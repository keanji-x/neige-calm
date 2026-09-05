// #1242 — the empty boundary live region costs a plugin row nothing.
//
// ## Why this is in the browser tier and cannot be anywhere else
//
// The region is mounted on **every** row from first paint, empty until that row
// has something to say, because a live region inserted together with its text
// is commonly not announced. That shape has a price the a11y fix does not pay
// for itself: an empty `<span>` is still a flex item, and `.pluginMeta` is a
// flex column with a `gap`. If nothing took the empty one out of flow, every
// row in the pane — not only a row someone toggled — would grow by exactly one
// gap, permanently.
//
// `.pluginEffectBoundary:empty { position: absolute }` is what pays it, and the
// claim is pure layout: an absolutely-positioned child of a flex container does
// not participate in flex layout, so `gap` does not apply to it. jsdom has no
// layout engine and reports every box as zero, so that claim is unfalsifiable
// in the `web-dom` tier — a test there could assert the element exists and the
// class is on it while the pane silently grew a gap under every row.
//
// So: measured geometry, and measured against two controls rather than one.
// Asserting only "empty region == region deleted" would also pass if the tape
// measure were broken and every number came back equal, which is the failure
// mode a geometry test has to rule out about itself. The in-flow control is
// what proves the measurement can see a gap at all.
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
      /* No row is flagged, which is the state every row of a freshly opened
         pane is in — and the state the empty region has to be free in. */
      effectBoundaryIds={new Set()}
      onSetEnabled={vi.fn()}
      onOpenConfig={vi.fn()}
    />,
  );
  await new Promise<void>((resolve) => {
    requestAnimationFrame(() => requestAnimationFrame(() => { resolve(); }));
  });

  /*
   * Reached through the role, not the CSS-module class: `no-class-dom-query`
   * forbids the latter, and the region's parent is the meta column whose height
   * is the thing under measurement.
   */
  const region = container.querySelector('[role="status"]');
  expect(region).not.toBeNull();
  expect(region?.textContent).toBe('');
  const meta = region?.parentElement;
  expect(meta).not.toBeNull();
  if (region === null || meta === null || meta === undefined) return;

  const heightOf = () => meta.getBoundingClientRect().height;
  const gap = Number.parseFloat(getComputedStyle(meta).rowGap);
  expect(gap).toBeGreaterThan(0);

  const asShipped = heightOf();

  /*
   * Control 1 — the region put back into the flow, which is what the pane looks
   * like without the `:empty` rule. Written as an inline `position: static`
   * override rather than by editing the stylesheet, so it cannot leak past this
   * assertion.
   */
  (region as HTMLElement).style.position = 'static';
  const inFlow = heightOf();
  (region as HTMLElement).style.removeProperty('position');
  expect(heightOf()).toBe(asShipped);

  // Control 2 — the region gone entirely: the height a row would have if this
  // feature had never been added.
  region.remove();
  const withoutRegion = heightOf();

  /*
   * The claim, both halves.
   *
   * Shipped == absent: mounting the region costs the row nothing, so no row in
   * the pane moved when this feature landed.
   */
  expect(asShipped).toBe(withoutRegion);
  /*
   * And the measurement can see a gap: in flow the same region costs exactly
   * one `row-gap`. Without this half, the assertion above would be satisfied by
   * a tape measure that always returns the same number.
   */
  expect(inFlow - asShipped).toBeCloseTo(gap, 1);
});
