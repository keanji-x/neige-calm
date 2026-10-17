import { cleanup, render } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { page } from 'vitest/browser';
import '../../styles/entry.css';
import { Drawer } from './public.tsx';
import { readingPlaceIn, restoreReadingPlace } from './reading-place.ts';
import { scrollGeometry } from './scroll-geometry.ts';

afterEach(cleanup);

it.each(['border-box', 'content-box'] as const)('maps fractional %s scroll geometry through nested scale and translation', boxSizing => {
  render(<div style={{ transform: 'translateY(17px) scale(.8)' }}>
    <div style={{ transform: 'scale(.9)' }}>
      <div data-pane style={{ boxSizing, height: 300.375, padding: 7, border: '3px solid', overflow: 'auto' }}>
        <div style={{ height: 20000 }} /><div data-target style={{ height: 20 }} /><div style={{ height: 1000 }} />
      </div>
    </div>
  </div>);
  const pane = document.querySelector<HTMLElement>('[data-pane]')!;
  const target = document.querySelector<HTMLElement>('[data-target]')!;
  pane.scrollTop += scrollGeometry(pane).y(target.getBoundingClientRect().top);
  // Independent painted-space oracle, including the scaled 3px border.
  expect(target.getBoundingClientRect().top - pane.getBoundingClientRect().top).toBeCloseTo(3 * .8 * .9, 1);
});

it('restores the same reading mark while the production drawer is entering', async () => {
  await page.viewport(1400, 900);
  render(<div style={{ height: 600, width: 900, position: 'relative' }}>
    <Drawer open title="Reading" onClose={() => {}}>
      {Array.from({ length: 100 }, (_, index) => <p key={index} style={{ height: 40 }}>Paragraph {index}</p>)}
    </Drawer>
  </div>);
  for (const animation of document.querySelector<HTMLElement>('[data-nc-drawer]')!.getAnimations()) {
    animation.pause(); animation.currentTime = 0;
  }
  const pane = document.querySelector<HTMLElement>('[data-nc-drawer-scroll]')!;
  pane.scrollTop = 1000;
  const place = readingPlaceIn(pane);
  expect(place.mark).not.toBeNull();
  const top = place.mark!.getBoundingClientRect().top;
  pane.scrollTop += 1500;
  restoreReadingPlace(pane, place);
  expect(place.mark!.getBoundingClientRect().top).toBeCloseTo(top, 0);
});
