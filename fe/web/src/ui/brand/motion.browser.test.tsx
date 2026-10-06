import { cleanup, render } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';
import { commands } from 'vitest/browser';

import '../../styles/entry.css';
import { NeigeMotion } from './motion.tsx';

declare module 'vitest/browser' { interface BrowserCommands { emulateReducedMotion(reduce: boolean): Promise<void> } }

afterEach(cleanup);

function mount(kind: 'thinking' | 'execution' | 'creation') {
  const { container } = render(<div style={{ width: 64, height: 64 }}><NeigeMotion kind={kind} /></div>);
  const svg = container.querySelector('svg')!;
  svg.pauseAnimations();
  return svg;
}

function folds(svg: SVGSVGElement) {
  return [...svg.lastElementChild!.querySelectorAll('polyline')];
}

function coordinates(polyline: SVGPolylineElement): readonly (readonly [number, number])[] {
  const matrix = polyline.getCTM()!;
  return Array.from({ length: polyline.animatedPoints.numberOfItems }, (_, index) => {
    const point = polyline.animatedPoints.getItem(index).matrixTransform(matrix);
    return [point.x, point.y] as const;
  });
}

describe('Neige vector motion in the browser', () => {
  it('rotates a closed triangle into the snow crystal without bending or stretching its folds', () => {
    const svg = mount('creation');
    svg.setCurrentTime(0);
    const paths = folds(svg);
    const triangle = paths.map(coordinates);
    for (let index = 0; index < 3; index++) {
      const end = triangle[index][2];
      const next = triangle[(index + 1) % 3][0];
      expect(Math.hypot(end[0] - next[0], end[1] - next[1])).toBeLessThan(.5);
    }
    const lengths = triangle.map(points => [0, 2].map(end => Math.hypot(points[end][0] - points[1][0], points[end][1] - points[1][1])));
    for (const time of [.9, 1.8, 2.8, 3.9, 5.59]) {
      svg.setCurrentTime(time);
      paths.map(coordinates).forEach((points, index) => {
        [0, 2].forEach((end, leg) => expect(Math.hypot(points[end][0] - points[1][0], points[end][1] - points[1][1]))
          .toBeCloseTo(lengths[index][leg], 3));
      });
    }
    svg.setCurrentTime(2.8);
    expect(paths.map(coordinates)).not.toEqual(triangle);
    svg.setCurrentTime(5.6);
    paths.map(coordinates).forEach((points, index) => points.forEach((point, vertex) => {
      expect(point[0]).toBeCloseTo(triangle[index][vertex][0], 3);
      expect(point[1]).toBeCloseTo(triangle[index][vertex][1], 3);
    }));
  });

  it('moves the three thinking seeds, grows from their centers, and returns along the same path', () => {
    const svg = mount('thinking');
    const circles = [...svg.lastElementChild!.querySelectorAll('circle')];
    svg.setCurrentTime(0);
    const start = circles.map(circle => circle.getCTM()!.e);
    svg.setCurrentTime(.5);
    expect(circles.map(circle => circle.getCTM()!.e)).not.toEqual(start);
    for (const time of [1.4, 2.1, 2.8]) {
      svg.setCurrentTime(time);
      const outward = folds(svg).map(coordinates);
      svg.setCurrentTime(5.6 - time);
      folds(svg).map(coordinates).forEach((points, index) => points.forEach((point, vertex) => {
        expect(point[0]).toBeCloseTo(outward[index][vertex][0], 3);
        expect(point[1]).toBeCloseTo(outward[index][vertex][1], 3);
      }));
    }
    svg.setCurrentTime(2.8);
    folds(svg).forEach((fold, index) => {
      const center = coordinates(fold).reduce((sum, [x]) => sum + x, 0) / 3;
      expect(center).toBeCloseTo(circles[index].getCTM()!.e, 3);
    });
  });

  it('changes the terminal chevron itself into the snow fold and back', () => {
    const svg = mount('execution');
    const paths = folds(svg);
    svg.setCurrentTime(0);
    const terminal = paths.map(coordinates);
    expect(terminal[0]).toEqual(terminal[1]);
    expect(terminal[1]).toEqual(terminal[2]);
    svg.setCurrentTime(1.8);
    const middle = paths.map(coordinates);
    expect(middle).not.toEqual(terminal);
    svg.setCurrentTime(2.8);
    const snow = paths.map(coordinates);
    expect(middle).not.toEqual(snow);
    expect(snow[0][0][0]).toBeCloseTo(32, 3);
    expect(snow[0][0][1]).toBeCloseTo(6, 3);
    svg.setCurrentTime(5.6);
    expect(paths.map(coordinates)).toEqual(terminal);
  });

  it('shows a static theme-owned mark when reduced motion is requested', async () => {
    await commands.emulateReducedMotion(true);
    try {
      for (const kind of ['thinking', 'execution', 'creation'] as const) {
        const svg = mount(kind);
        expect(getComputedStyle(svg.lastElementChild!).display).toBe('none');
        expect(getComputedStyle(svg.firstElementChild!).display).not.toBe('none');
        expect(svg.getAttribute('stroke')).toBe('currentColor');
      }
    } finally { await commands.emulateReducedMotion(false); }
  });
});

it('turns each creation fold clockwise throughout the cycle', () => {
  const svg = mount('creation');
  const paths = folds(svg);
  const previous = paths.map(() => 0);
  for (let step = 0; step <= 55; step++) {
    svg.setCurrentTime(step / 10);
    paths.forEach((path, index) => {
      const matrix = path.getCTM()!;
      const angle = Math.atan2(matrix.b, matrix.a) * 180 / Math.PI;
      if (step > 0) {
        const delta = ((angle - previous[index] + 540) % 360) - 180;
        expect(delta).toBeGreaterThanOrEqual(-.001);
      }
      previous[index] = angle;
    });
  }
});

it('shares one native cycle across all tracks and removes the SVG on unmount', () => {
  for (const kind of ['thinking', 'execution', 'creation'] as const) {
    const { container, unmount } = render(<NeigeMotion kind={kind} />);
    const svg = container.querySelector('svg')!;
    const tracks = [...svg.querySelectorAll('animate, animateTransform')];
    expect(tracks.length).toBeGreaterThan(0);
    expect(tracks.every(track => track.getAttribute('dur') === '5.6s' && track.getAttribute('repeatCount') === 'indefinite')).toBe(true);
    unmount();
    expect(svg.isConnected).toBe(false);
    expect(document.querySelector('[data-nc-motion]')).toBeNull();
  }
});
