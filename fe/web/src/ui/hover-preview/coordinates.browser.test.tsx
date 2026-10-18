import { act, cleanup, render } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { commands, page } from 'vitest/browser';
import '../../styles/entry.css';
import { placePreview } from './placement.ts';
import { HoverPreview } from './public.tsx';

const physical = ({ left, top, right, bottom, width, height }: DOMRectReadOnly) => ({ left, top, right, bottom, width, height });
const frames = async (count: number) => { for (let i = 0; i < count; i++) await new Promise(requestAnimationFrame); };
afterEach(async () => { cleanup(); vi.restoreAllMocks(); await commands.emulateReducedMotion(false); });

type Case = Readonly<{ layers: number; shift: number; width?: number; gap?: number; scale?: number; zoom?: number; inline?: boolean }>;
async function checkCoordinates({ layers, shift, width = 1440, gap = 34, scale = 1, zoom = 1, inline = false }: Case) {
  await page.viewport(width, 4000);
  const activate: (() => void)[] = [];
  function Level({ index }: { index: number }) {
    return <HoverPreview title={`L${index}`} trigger={open => { activate[index] = open; return <button>{`L${index}`}</button>; }}>
      <div style={{ height: index === 0 ? 30 : gap }} />
      <div style={{ transform: `translateY(${shift}px) scale(${scale})`, transformOrigin: '0 0', zoom }}>
        {inline && <span>Wrapping inline source words with multiple real lines: </span>}
        {index + 1 < layers && <Level index={index + 1} />}
        {inline && <span> trailing reading text</span>}
      </div><div style={{ height: 800 }} />
    </HoverPreview>;
  }
  function Tree() { return <div style={{ position: 'absolute', left: Math.min(950, width - 90), top: 140 }}><Level index={0} /></div>; }
  const cards = () => Array.from(document.querySelectorAll<HTMLElement>('[role="dialog"]'));
  const slots = () => cards().map(card => card.closest<HTMLElement>('[data-nc-preview-slot]')!.getBoundingClientRect());
  const open = async () => {
    for (let i = 0; i < layers; i++) { act(() => { activate[i](); }); await frames(5); }
    expect(cards()).toHaveLength(layers);
  };
  const assigned = new WeakMap<Animation, number>();
  const start = Object.getOwnPropertyDescriptor(Animation.prototype, 'startTime')!;
  vi.spyOn(Animation.prototype, 'startTime', 'set').mockImplementation(function (this: Animation, value) {
    if (typeof value === 'number') assigned.set(this, value);
    start.set!.call(this, value);
  });
  let phase = Array<number>(layers).fill(0);
  const native = Object.getOwnPropertyDescriptor(Element.prototype, 'animate')!.value as Element['animate'];
  vi.spyOn(Element.prototype, 'animate').mockImplementation(function (this: Element, ...args) {
    const animation = native.apply(this, args);
    void animation.ready.then(() => {
      if (animation.playState === 'idle') return;
      expect(assigned.has(animation)).toBe(true);
      animation.pause(); animation.currentTime = phase[cards().indexOf(this as HTMLElement)] ?? 0;
    }, () => {});
    return animation;
  });
  render(<Tree />); await open();
  // Independent physical inputs, through the sole placement and an empty CSS
  // ruler. Never invert paint, suppress transforms or write a production slot.
  const edgeSample = () => cards().map(card => {
    const anchor = document.getElementById(card.dataset.ncPreviewOwner!)!;
    const slot = card.closest<HTMLElement>('[data-nc-preview-slot]')!;
    const owner = anchor.closest<HTMLElement>('[data-nc-preview-owner]');
    const obstacles = Array.from(document.querySelectorAll<HTMLElement>('[data-nc-preview-slot]'))
      .filter(element => element !== slot && !element.inert
        && Boolean(element.compareDocumentPosition(slot) & Node.DOCUMENT_POSITION_FOLLOWING))
      .map(element => element.getBoundingClientRect());
    const viewport = { width: innerWidth, height: innerHeight };
    const source = anchor.getBoundingClientRect(); const reading = owner?.getBoundingClientRect() ?? null;
    const reference = placePreview({ anchor: source, reading: reading, obstacles, viewport });
    expect(reference).not.toBeNull();
    const actual = slot.getBoundingClientRect();
    // Empty fixed ruler commits the placement through real CSS without writing
    // the production slot. Serializing/restoring style.top would lose the very
    // fractional CSS input under test and could move the current parent frame.
    const ruler = document.createElement('div');
    Object.assign(ruler.style, { position: 'fixed', left: `${reference!.x}px`, top: `${reference!.y}px`,
      width: `${reference!.width}px`, height: `${reference!.maxHeight}px` });
    document.body.append(ruler);
    const committed = ruler.getBoundingClientRect(); ruler.remove();
    const edges = (['left', 'top', 'right', 'bottom'] as const).map(edge => ({ edge,
      slot: actual[edge] - committed[edge],
    }));
    return { edges, input: { source: physical(source), reading: reading === null ? null : physical(reading), obstacles: obstacles.map(physical), viewport }, actual: physical(actual) };
  });
  const edgeSamples: ReturnType<typeof edgeSample>[] = [];
  const samples: DOMRect[][] = [];
  const phases: number[] = [];

  for (let round = 0; round < 3; round++) for (const time of [...Array.from({ length: 21 }, (_, i) => i * 4), 0, 80, 40, 0, 80, 40]) {
    phase = [time, ...Array<number>(layers - 1).fill(0)];
    for (const [index, card] of cards().entries()) {
      const effects = card.getAnimations(); expect(effects).toHaveLength(1);
      effects[0].pause(); await effects[0].ready; effects[0].currentTime = phase[index];
    }
    act(() => { window.dispatchEvent(new Event('resize')); }); await frames(6);
    for (const [index, card] of cards().entries()) {
      const effects = card.getAnimations(); expect(effects).toHaveLength(1);
      expect(effects[0].playState).toBe('paused'); expect(effects[0].pending).toBe(false);
      expect(effects[0].currentTime).toBe(phase[index]); expect(assigned.has(effects[0])).toBe(true);
    }
    const held = edgeSample();
    await frames(2);
    const repeat = edgeSample();
    // Holding the complete physical input (not just root progress) must stop
    // work exactly. Deep paint compensation is part of the physical input.
    expect(repeat).toEqual(held);
    for (const snapshot of [held, repeat]) {
      samples.push(slots()); edgeSamples.push(snapshot); phases.push(time);
    }
  }
  const request = vi.spyOn(window, 'requestAnimationFrame');
  const changes: MutationRecord[] = []; let notices = 0;
  const observer = new MutationObserver(records => changes.push(...records));
  cards().forEach(card => observer.observe(card.closest('[data-nc-preview-slot]')!, { attributes: true, subtree: true }));
  const notice = () => { notices++; };
  document.addEventListener('nc-preview-layout', notice);
  const idle: string[] = [];
  for (let i = 0; i < 20; i++) { await frames(1); idle.push(JSON.stringify(slots().map(({ left, top, right, bottom }) => ({ left, top, right, bottom })))); }
  observer.disconnect(); document.removeEventListener('nc-preview-layout', notice);
  expect(request).toHaveBeenCalledTimes(20);
  expect(changes).toEqual([]); expect(notices).toBe(0); expect(new Set(idle).size).toBe(1);
  const results = Array.from({ length: layers }, (_, layer) => {
    const edges = (['left', 'top', 'right', 'bottom'] as const).map(edge => {
      const values = samples.map(sample => sample[layer][edge]);
      return { edge, trajectory: Math.max(...values.map(value => Math.abs(value - samples[0][layer][edge]))),
        span: Math.max(...values) - Math.min(...values) };
    });
    return { layer: layer + 1, edges };
  });
  console.info('coordinate evidence', JSON.stringify({ layers, shift, rounds: 3, samples: samples.length, results }));
  const directEdges = Array.from({ length: layers }, (_, layer) => ({ layer: layer + 1,
    edges: (['left', 'top', 'right', 'bottom'] as const).map((edge, index) => ({ edge,
      errors: (['slot'] as const).map(kind => {
        const values = edgeSamples.map(sample => sample[layer].edges[index][kind]);
        return { kind, absolute: Math.max(...values.map(Math.abs)), span: Math.max(...values) - Math.min(...values) };
      }),
    })),
  }));
  const revisits = Array.from({ length: layers }, (_, layer) => ({ layer: layer + 1,
    edges: (['left', 'top', 'right', 'bottom'] as const).map(edge => {
      const first = new Map<string, number>();
      const differences = samples.map((sample, index) => {
        const value = sample[layer][edge];
        const key = JSON.stringify({ phase: phases[index], input: edgeSamples[index][layer].input });
        if (!first.has(key)) first.set(key, value);
        return value - first.get(key)!;
      });
      return { edge, absolute: Math.max(...differences.map(Math.abs)), span: Math.max(...differences) - Math.min(...differences) };
    }),
  }));
  console.info('physical trajectory evidence', JSON.stringify(edgeSamples.filter((_, index) => index < 2 || phases[index] === 80).map((edges, index) => ({ index, edges }))));
  console.info('direct edge evidence', JSON.stringify({ layers, shift, width, gap, scale, zoom, inline,
    rounds: 3, phases: 21, samples: samples.length, directEdges, totals: results, revisits }));
  for (const result of revisits) for (const { edge, absolute, span } of result.edges) {
    expect.soft(absolute, `${result.layer}:${edge}:revisit`).toBe(0);
    expect.soft(span, `${result.layer}:${edge}:revisit-span`).toBe(0);
  }
  for (const result of directEdges) for (const { edge, errors } of result.edges) for (const { kind, absolute, span } of errors) {
    expect.soft(absolute, `${result.layer}:${edge}:${kind}:absolute`).toBeLessThanOrEqual(1 / 64);
    expect.soft(span, `${result.layer}:${edge}:${kind}:span`).toBeLessThanOrEqual(1 / 64);
  }
}

it('commits three-level visual inputs and revisits phases without drift', () => checkCoordinates({ layers: 3, shift: 0 }), 45_000);
it('commits five-level fractional visual inputs against independent CSS rulers', () => checkCoordinates({ layers: 5, shift: -1 / 2048 }), 45_000);

it('commits five-level fine CSS visual inputs against independent CSS rulers', () => checkCoordinates({ layers: 5, shift: -3 / 131072 }), 45_000);

for (const width of [390, 1440]) for (const mode of ['scale', 'zoom'] as const) for (const factor of [0.75, 1, 1.125, 1.5]) {
  it(`commits visual edges at ${width} ${mode} ${factor}`, () => checkCoordinates({ layers: 5,
    shift: 1 / 2048, width, [mode]: factor, inline: true }), 45_000);
}
for (const gap of [33.984375, 33.999, 34.007, 34.015625, 34.03125]) {
  it(`commits visual edges across gap boundary ${gap}`, () => checkCoordinates({ layers: 3, shift: 0, gap }), 45_000);
}
