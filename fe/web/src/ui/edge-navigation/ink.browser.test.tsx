import { act, cleanup, fireEvent, render } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { page } from 'vitest/browser';
import '../../styles/entry.css';
import { EdgeNavigator } from './public.tsx';

const track = () => document.querySelector<HTMLElement>('[data-nc-rail-track]')!;
const dots = () => [...track().querySelectorAll<HTMLButtonElement>('button')];
const ink = () => dots().map(dot => Number(dot.style.getPropertyValue('--nc-dot-proximity')));
async function frames() {
  for (let i = 0; i < 3; i++) await act(async () => {
    await new Promise<void>(resolve => requestAnimationFrame(() => resolve()));
  });
}
function rail(count: number) {
  return <div style={{ height: 600, width: 24 }}><EdgeNavigator label="Sections" activeId="0" onSelect={() => {}}
    items={Array.from({ length: count }, (_, index) => ({ id: String(index), label: `Section ${index}`, title: `Section ${index}`, readExcerpt: () => '' }))} /></div>;
}
afterEach(async () => { cleanup(); vi.restoreAllMocks(); await page.viewport(1280, 800); });

it('bounds pointer geometry and ink writes independently of long rail length', async () => {
  await page.viewport(1400, 900);
  render(rail(256)); await frames();
  track().scrollTop = 3900; await frames();
  const target = dots()[200];
  const box = target.getBoundingClientRect();
  let reads = 0;
  const native = Reflect.get(HTMLElement.prototype, 'getBoundingClientRect');
  vi.spyOn(HTMLElement.prototype, 'getBoundingClientRect').mockImplementation(function (this: HTMLElement) {
    if (this.parentElement?.hasAttribute('data-nc-rail-track')) reads++;
    return native.call(this);
  });
  const set = vi.spyOn(CSSStyleDeclaration.prototype, 'setProperty');
  const remove = vi.spyOn(CSSStyleDeclaration.prototype, 'removeProperty');
  fireEvent.pointerMove(track(), { pointerType: 'mouse', clientY: box.top + box.height / 2 });
  await frames();
  expect(ink()[200]).toBe(1);
  expect(ink()[199]).toBeGreaterThan(ink()[198]);
  expect(ink()[198]).toBeGreaterThan(0);
  expect(ink()[190]).toBe(0);
  const writes = () => set.mock.calls.filter(([name]) => name === '--nc-dot-proximity').length
    + remove.mock.calls.filter(([name]) => name === '--nc-dot-proximity').length;
  expect.soft(reads).toBeLessThanOrEqual(4);
  expect.soft(writes()).toBeLessThanOrEqual(24);
  reads = 0; set.mockClear(); remove.mockClear();
  fireEvent.pointerMove(track(), { pointerType: 'mouse', clientY: box.top + box.height * 8.5 });
  await frames();
  expect(ink()[208]).toBe(1);
  expect(ink()[200]).toBe(0);
  expect.soft(reads).toBeLessThanOrEqual(4);
  expect(writes()).toBeLessThanOrEqual(24);
  fireEvent.pointerLeave(track()); await frames();
  expect(ink().every(value => value === 0)).toBe(true);
});

it('uses live fractional pitch and scroll position for a stationary pointer', async () => {
  await page.viewport(1400, 900);
  render(rail(40)); await frames();
  const box = dots()[4].getBoundingClientRect();
  const at = box.top + box.height / 2;
  fireEvent.pointerMove(track(), { pointerType: 'mouse', clientY: at }); await frames();
  expect(ink()[4]).toBe(1);
  track().scrollTop = 40; fireEvent.scroll(track()); await frames();
  expect(ink()[6]).toBe(1);
  const group = document.querySelector<HTMLElement>('[role="group"][aria-label="Sections"]')!;
  group.style.setProperty('--nc-rail-pitch', '37.5px');
  window.dispatchEvent(new Event('resize')); await frames();
  const boxes = dots().map(dot => dot.getBoundingClientRect());
  expect(boxes[0].height).toBe(37.5);
  const distances = boxes.map(box => Math.abs(box.top + box.height / 2 - at));
  const nearest = distances.indexOf(Math.min(...distances));
  const values = ink();
  expect(values.indexOf(Math.max(...values))).toBe(nearest);
  fireEvent.pointerCancel(track()); await frames();
  expect(ink().every(value => value === 0)).toBe(true);
});

it('clears owned ink after item replacement, empty rails and unmount', async () => {
  await page.viewport(1400, 900);
  const view = render(rail(8)); await frames();
  const box = dots()[4].getBoundingClientRect();
  fireEvent.pointerMove(track(), { pointerType: 'mouse', clientY: box.top + box.height / 2 }); await frames();
  const previous = dots(); expect(ink()[4]).toBe(1);
  view.rerender(rail(0)); await frames();
  expect(previous.every(dot => dot.style.getPropertyValue('--nc-dot-proximity') === '')).toBe(true);
  view.rerender(rail(8)); await frames();
  const next = dots(); const nextBox = next[4].getBoundingClientRect();
  fireEvent.pointerMove(track(), { pointerType: 'mouse', clientY: nextBox.top + nextBox.height / 2 }); await frames();
  expect(ink()[4]).toBe(1);
  view.unmount();
  expect(next.every(dot => dot.style.getPropertyValue('--nc-dot-proximity') === '')).toBe(true);
});
