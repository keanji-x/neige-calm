import { waitFor } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';

import { ARRIVAL_ATTRIBUTE, revealReportAnchor } from './public.ts';

afterEach(() => { document.body.replaceChildren(); });

const nextFrame = () => new Promise<void>((resolve) => { requestAnimationFrame(() => resolve()); });

it('starts the arrival marker only after a distant smooth-scroll target has settled in view', async () => {
  const scroller = document.createElement('div');
  scroller.style.cssText = 'height:300px;overflow-y:auto';
  scroller.innerHTML = '<article data-nc-report=""><div style="height:3000px"></div><h2 id="far">Far section</h2><div style="height:300px"></div></article>';
  document.body.append(scroller);
  const target = document.getElementById('far')!;
  let topAtArrival: number | null = null;
  const observer = new MutationObserver(() => {
    if (target.hasAttribute(ARRIVAL_ATTRIBUTE)) topAtArrival = target.getBoundingClientRect().top;
  });
  observer.observe(target, { attributes: true, attributeFilter: [ARRIVAL_ATTRIBUTE] });

  revealReportAnchor('far', document, 'smooth');
  await nextFrame();
  expect(target.hasAttribute(ARRIVAL_ATTRIBUTE)).toBe(false);

  await waitFor(() => expect(topAtArrival).not.toBeNull(), { timeout: 3_000 });
  expect(Math.abs((topAtArrival ?? Infinity) - scroller.getBoundingClientRect().top)).toBeLessThan(2);
  observer.disconnect();
});
