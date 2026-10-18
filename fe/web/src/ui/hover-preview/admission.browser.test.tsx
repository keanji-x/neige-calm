import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import { useLayoutEffect, useRef } from 'react';
import { commands, page } from 'vitest/browser';
import { afterEach, expect, it, vi } from 'vitest';
import '../../styles/entry.css';
import { HoverPreview } from './public.tsx';
import { readSurfaceFrame } from '../motion/geometry.ts';
vi.mock('../motion/geometry.ts', { spy: true });
afterEach(async () => { cleanup(); vi.mocked(readSurfaceFrame).mockReset(); vi.restoreAllMocks(); await commands.emulateReducedMotion(false); });
type Proof = { kind: 'pending' | 'revoked' } | { kind: 'admitted'; token: object; read: () => boolean };
const frames = async (count: number) => { for (let i = 0; i < count; i++) await new Promise(requestAnimationFrame); };

for (const reduced of [false, true]) {
  const mode = reduced ? 'reduced' : 'normal';
  it(`prepares without global identity and rejects synthetic activation (${mode})`, async () => {
    await page.viewport(1200, 900); await commands.emulateReducedMotion(reduced);
    const { readSurfaceFrame: realFrame } = await vi.importActual<typeof import('../motion/geometry.ts')>('../motion/geometry.ts');
    vi.mocked(readSurfaceFrame).mockImplementation(element => element?.querySelector(':scope > [aria-label="Preview: Pending"]') ? null : realFrame(element));
    const action = vi.fn();
    render(<div style={{ padding: 80 }}><HoverPreview title="Owner" trigger={activate => <button onClick={activate}>Owner</button>}>
      <HoverPreview title="Pending" trigger={activate => <button onClick={activate}>Pending</button>}><button onClick={action}>Action</button></HoverPreview>
    </HoverPreview></div>);
    act(() => { fireEvent.click(screen.getByRole('button', { name: 'Owner' })); });
    act(() => { fireEvent.click(screen.getByRole('button', { name: 'Pending' })); });
    const pending = document.querySelector<HTMLElement>('[aria-label="Preview: Pending"]')!;
    expect(pending.inert).toBe(true);
    expect(pending.getAttribute('aria-hidden')).toBe('true');
    expect(pending.hasAttribute('data-nc-link-preview')).toBe(false);
    expect(pending.hasAttribute('data-nc-escape-layer')).toBe(false);
    act(() => { fireEvent.click(pending.querySelector('button')!); });
    expect(action).not.toHaveBeenCalled();
    const escape = new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true });
    act(() => { document.dispatchEvent(escape); });
    expect(escape.defaultPrevented).toBe(true);
    expect(document.querySelector('[data-nc-link-preview]')).toBeNull();
    expect(pending.isConnected).toBe(false);
  });

  it(`publishes readable bindings and advances three requested levels from one input (${mode})`, async () => {
    await page.viewport(1800, 1000); await commands.emulateReducedMotion(reduced);
    const { readSurfaceFrame: realFrame } = await vi.importActual<typeof import('../motion/geometry.ts')>('../motion/geometry.ts');
    let blocked = true;
    vi.mocked(readSurfaceFrame).mockImplementation(element => blocked && element?.querySelector(':scope > [aria-label="Preview: Root"]') ? null : realFrame(element));
    const opens: (() => void)[] = [];
    const branch = (depth: number): React.ReactNode => <HoverPreview title={['Root', 'Child', 'Leaf'][depth]}
      trigger={activate => { opens[depth] = activate; return <button>Level {depth}</button>; }}>
      {depth < 2 ? branch(depth + 1) : <button>Leaf action</button>}
    </HoverPreview>;
    render(<div style={{ padding: 80 }}>{branch(0)}</div>);
    // Explicit public callbacks model requests while ancestors prepare. This is
    // a measurement-boundary regression, not a natural pointer reachability claim.
    for (let depth = 0; depth < 3; depth++) act(() => { opens[depth](); });
    await frames(4);
    const notices: (Proof | null)[] = [];
    const register = (event: Event) => {
      const slot = event.target as HTMLElement;
      const card = slot.querySelector<HTMLElement>('[role=dialog]');
      if (!card?.hasAttribute('data-nc-link-preview')) return;
      notices.push(readPort(card));
    };
    document.addEventListener('nc-preview-layout', register);
    blocked = false;
    act(() => { window.dispatchEvent(new Event('resize')); });
    await frames(12);
    document.removeEventListener('nc-preview-layout', register);
    expect(notices.length).toBeGreaterThanOrEqual(3);
    expect(notices.every(bounds => bounds !== null)).toBe(true);
    expect(document.querySelectorAll('[data-nc-link-preview]')).toHaveLength(3);
    await expect.poll(() => document.getAnimations().length).toBe(0); await frames(4);
    const mutations: MutationRecord[] = [];
    const observer = new MutationObserver(records => mutations.push(...records));
    observer.observe(document.body, { attributes: true, subtree: true, childList: true });
    await frames(20); observer.disconnect();
    expect(mutations).toHaveLength(0);
  });
}

// Internal typed owner endpoint, read synchronously inside the observer under
// test. It never supplies geometry or advances the production lifecycle.
function readPort(card: HTMLElement) {
  const received: { current: Proof } = { current: { kind: 'pending' } };
  const child = { release: null as null | (() => void), setParent: (proof: Proof) => { received.current = proof; } };
  document.getElementById(card.dataset.ncPreviewOwner!)!.dispatchEvent(new CustomEvent('nc-preview-register', { detail: child }));
  child.release?.();
  const current = received.current;
  return current.kind === 'admitted' && current.read() ? current : null;
}
for (const reduced of [false, true]) {
  it(`publishes same-frame motion-only recovery and atomic focus before becoming idle (${reduced ? 'reduced' : 'normal'})`, async () => {
    await page.viewport(1200, 900); await commands.emulateReducedMotion(reduced);
    const { readSurfaceFrame: realFrame } = await vi.importActual<typeof import('../motion/geometry.ts')>('../motion/geometry.ts');
    let available = true;
    vi.mocked(readSurfaceFrame).mockImplementation(element => available ? realFrame(element) : null);
    const focusReads: boolean[] = []; const action = vi.fn();
    render(<div style={{ padding: 80 }} onFocus={event => {
      const card = (event.target as HTMLElement).closest<HTMLElement>('[data-nc-preview-owner]');
      if (card !== null) focusReads.push(!card.inert && !card.hasAttribute('aria-hidden') && card.hasAttribute('data-nc-link-preview') && readPort(card) !== null);
    }}><HoverPreview title="Recover" trigger={activate => <button onClick={activate}>Recover</button>}><button onClick={action}>Action</button></HoverPreview>
      <div data-testid="scroll-input" />
    </div>);
    act(() => { fireEvent.keyDown(screen.getByRole('button', { name: 'Recover' }), { key: 'ArrowDown' }); });
    const card = screen.getByRole('dialog'); expect(focusReads).toEqual([true]);
    card.getAnimations().forEach(animation => { animation.pause(); animation.currentTime = 80; });
    await frames(4);
    const notices: boolean[] = [];
    let previous: object | null | undefined;
    const listener = () => { if (card.isConnected) {
      const proof = readPort(card); const token = proof?.token ?? null;
      if (token !== previous) { previous = token; notices.push(proof !== null); }
    } };
    document.addEventListener('nc-preview-layout', listener);
    available = false;
    act(() => { fireEvent.click(card.querySelector('button')!); });
    expect(action).not.toHaveBeenCalled();
    // A scroll in an unrelated node reaches only motion's geometry rAF. Hover
    // has no placement/height update and does not close for this scroll owner.
    act(() => { fireEvent.scroll(screen.getByTestId('scroll-input')); }); await frames(4);
    expect(card.inert).toBe(true); expect(card.hasAttribute('data-nc-link-preview')).toBe(false);
    expect(getComputedStyle(card.parentElement!).clipPath).toBe('inset(50%)'); expect(readPort(card)).toBeNull();
    available = true;
    act(() => { fireEvent.scroll(screen.getByTestId('scroll-input')); }); await frames(4);
    expect(card.inert).toBe(false); expect(readPort(card)).not.toBeNull();
    expect(notices).toEqual([false, true]);
    await expect.poll(() => card.getAnimations().length).toBe(0);
    await frames(20); expect(notices).toEqual([false, true]);
    document.removeEventListener('nc-preview-layout', listener);
  });

  it(`cancels local preparation and revokes old descendant requests across a same-commit reopen (${reduced ? 'reduced' : 'normal'})`, async () => {
    await page.viewport(1800, 1000); await commands.emulateReducedMotion(reduced);
    let reopen: () => void = () => {}; let dismiss: () => void = () => {};
    render(<div style={{ padding: 80 }}><HoverPreview title="Generation" trigger={(activate, close) => {
      reopen = activate; dismiss = close; return <button onClick={activate}>Generation</button>;
    }}><HoverPreview title="Old child" trigger={activate => <button onClick={activate}>Old child</button>}><button>Old action</button></HoverPreview></HoverPreview></div>);
    act(() => { reopen(); });
    const childTrigger = screen.getByRole('button', { name: 'Old child' });
    const { readSurfaceFrame: realFrame } = await vi.importActual<typeof import('../motion/geometry.ts')>('../motion/geometry.ts');
    vi.mocked(readSurfaceFrame).mockImplementation(element => element?.querySelector(':scope > [aria-label="Preview: Old child"]') ? null : realFrame(element));
    act(() => { fireEvent.click(childTrigger); });
    const preparing = document.querySelector('[aria-label="Preview: Old child"]')!;
    act(() => { fireEvent.keyDown(childTrigger, { key: 'Escape' }); });
    expect(preparing.isConnected).toBe(false); expect(screen.getByRole('dialog').inert).toBe(false);
    vi.mocked(readSurfaceFrame).mockImplementation(realFrame);
    act(() => { fireEvent.keyDown(childTrigger, { key: 'ArrowDown' }); });
    const child = screen.getByRole('dialog', { name: 'Preview: Old child' });
    const parent = screen.getByRole('dialog', { name: 'Preview: Generation' });
    const old = child.getAnimations()[0]; if (old) { old.pause(); old.currentTime = 80; }
    act(() => { dismiss(); reopen(); });
    expect(parent.isConnected).toBe(true); expect(parent.inert).toBe(false);
    if (!reduced) { expect(child.isConnected).toBe(true); expect(child.inert).toBe(true); }
    child.getAnimations().forEach(animation => animation.finish()); await frames(3);
    expect(document.querySelectorAll('[data-nc-link-preview]')).toHaveLength(1);
    expect(parent.isConnected).toBe(true);
    act(() => { fireEvent.keyDown(screen.getByRole('button', { name: 'Old child' }), { key: 'ArrowDown' }); });
    expect(screen.getByRole('dialog', { name: 'Preview: Old child' }).inert).toBe(false);
  });
}

for (const reduced of [false, true]) it(`fences descendant ports and global Escape against a currently unavailable ancestor (${reduced ? 'reduced' : 'normal'})`, async () => {
  await page.viewport(1800, 1000); await commands.emulateReducedMotion(reduced);
  render(<div style={{ padding: 80 }}><HoverPreview title="Fence parent" trigger={activate => <button onClick={activate}>Fence parent</button>}>
    <HoverPreview title="Fence child" trigger={activate => <button onClick={activate}>Fence child</button>}><button>Descendant action</button></HoverPreview>
  </HoverPreview></div>);
  act(() => { fireEvent.click(screen.getByRole('button', { name: 'Fence parent' })); });
  act(() => { fireEvent.click(screen.getByRole('button', { name: 'Fence child' })); });
  const parent = screen.getByRole('dialog', { name: 'Preview: Fence parent' });
  const child = screen.getByRole('dialog', { name: 'Preview: Fence child' });
  const { readSurfaceFrame: realFrame } = await vi.importActual<typeof import('../motion/geometry.ts')>('../motion/geometry.ts');
  vi.mocked(readSurfaceFrame).mockImplementation(element => element === parent.parentElement ? null : realFrame(element));
  // Observe before a geometry event/notice can commit pending downstream. A
  // remembered admitted tag must not lend the child's port fresh authority.
  expect.soft(readPort(child)).toBeNull();
  const escape = new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true });
  act(() => { document.dispatchEvent(escape); });
  expect.soft(escape.defaultPrevented).toBe(false);
});

for (const reduced of [false, true]) it(`binds ancestor proofs to their publication and republishes descendants after the owner commit (${reduced ? 'reduced' : 'normal'})`, async () => {
  await page.viewport(1800, 1000); await commands.emulateReducedMotion(reduced);
  let open: () => void = () => {}; let close: () => void = () => {};
  function Fixture({ height }: { height: number }) {
    return <div style={{ padding: 80 }}><HoverPreview title="Proof parent" trigger={(activate, dismiss) => {
      open = activate; close = dismiss; return <button onClick={activate}>Proof parent</button>;
    }}><HoverPreview title="Proof child" trigger={activate => <button onClick={activate}>Proof child</button>}><p>Content</p></HoverPreview><div style={{ height }} /></HoverPreview></div>;
  }
  const fixture = render(<Fixture height={40} />);
  act(() => { open(); });
  act(() => { fireEvent.click(screen.getByRole('button', { name: 'Proof child' })); });
  const parent = screen.getByRole('dialog', { name: 'Preview: Proof parent' });
  const child = screen.getByRole('dialog', { name: 'Preview: Proof child' });
  parent.getAnimations().forEach(animation => animation.finish());
  child.getAnimations().forEach(animation => animation.finish()); await frames(4);
  const proofs: Extract<Proof, { kind: 'admitted' }>[] = []; const reads: boolean[] = [];
  const registration = { release: null as null | (() => void), setParent: (proof: Proof) => {
    if (proof.kind !== 'admitted') return;
    if (proofs.length > 0) reads.push(readPort(parent) !== null && readPort(child) === null);
    proofs.push(proof);
  } };
  document.getElementById(parent.dataset.ncPreviewOwner!)!.dispatchEvent(new CustomEvent('nc-preview-register', { detail: registration }));
  const first = proofs[0]; expect(first.read()).toBe(true);
  fixture.rerender(<Fixture height={80} />); await frames(4);
  expect.soft(proofs.length).toBeGreaterThan(1);
  expect.soft(reads.every(Boolean)).toBe(true);
  expect.soft(first.read()).toBe(false);
  const latest = proofs.at(-1)!;
  fixture.rerender(<Fixture height={80} />);
  expect.soft(latest.read()).toBe(true);
  act(() => { close(); open(); });
  expect.soft(latest.read()).toBe(false); expect(proofs.at(-1)!.read()).toBe(true);
  registration.release?.();
});

for (const reduced of [false, true]) it(`preserves a pending child focus request through ancestor inert suspension (${reduced ? 'reduced' : 'normal'})`, async () => {
  await page.viewport(1800, 1000); await commands.emulateReducedMotion(reduced);
  let parentBlocked = false; let childBlocked = true;
  const { readSurfaceFrame: realFrame } = await vi.importActual<typeof import('../motion/geometry.ts')>('../motion/geometry.ts');
  vi.mocked(readSurfaceFrame).mockImplementation(element => parentBlocked && element?.querySelector(':scope > [aria-label="Preview: Focus parent"]')
    || childBlocked && element?.querySelector(':scope > [aria-label="Preview: Focus child"]') ? null : realFrame(element));
  render(<div style={{ padding: 80 }}><HoverPreview title="Focus parent" trigger={activate => <button onClick={activate}>Focus parent</button>}>
    <HoverPreview title="Focus child" trigger={activate => <button onClick={activate}>Focus child</button>}><button>Child action</button></HoverPreview>
  </HoverPreview><div data-testid="focus-scroll-input" /></div>);
  act(() => { fireEvent.click(screen.getByRole('button', { name: 'Focus parent' })); });
  const trigger = screen.getByRole('button', { name: 'Focus child' });
  act(() => { trigger.focus(); fireEvent.keyDown(trigger, { key: 'ArrowDown' }); });
  const child = document.querySelector<HTMLElement>('[aria-label="Preview: Focus child"]')!;
  expect(document.activeElement).toBe(trigger); expect(child.inert).toBe(true);
  await frames(4); parentBlocked = true;
  act(() => { fireEvent.scroll(screen.getByTestId('focus-scroll-input')); }); await frames(20);
  expect(child.isConnected).toBe(true);
  parentBlocked = false; childBlocked = false;
  act(() => { fireEvent.scroll(screen.getByTestId('focus-scroll-input')); }); await frames(8);
  expect(document.querySelectorAll('[data-nc-link-preview]')).toHaveLength(2);
  expect(document.activeElement).toBe(screen.getByRole('region', { name: 'Preview content: Focus child' }));
});

for (const reduced of [false, true]) it(`rejects captured hover and leave callbacks from a revoked request generation (${reduced ? 'reduced' : 'normal'})`, async () => {
  await page.viewport(1800, 1000); await commands.emulateReducedMotion(reduced);
  const timer = vi.spyOn(window, 'setTimeout');
  let open: () => void = () => {}; let close: () => void = () => {};
  render(<div style={{ padding: 80 }}><HoverPreview title="Timer parent" trigger={(activate, dismiss) => {
    open = activate; close = dismiss; return <button onClick={activate}>Timer parent</button>;
  }}><HoverPreview title="Timer child" trigger={activate => <button onClick={activate}>Timer child</button>}><p>Child content</p></HoverPreview></HoverPreview></div>);
  act(() => { open(); });
  const trigger = screen.getByRole('button', { name: 'Timer child' }).parentElement!;
  act(() => { fireEvent.pointerEnter(trigger); });
  const openTimer = timer.mock.calls.filter(([, delay]) => delay === 300).at(-1)![0] as () => void;
  act(() => { close(); open(); });
  // Deliberate late-callback injection, not a claim that native clearTimeout
  // naturally delivers a cancelled callback. Execute the real captured owner.
  act(() => { openTimer(); });
  expect.soft(document.querySelector('[aria-label="Preview: Timer child"]')).toBeNull();
  act(() => { close(); open(); });
  act(() => { fireEvent.keyDown(screen.getByRole('button', { name: 'Timer child' }), { key: 'ArrowDown' }); });
  act(() => { fireEvent.pointerLeave(screen.getByRole('button', { name: 'Timer child' }).parentElement!); });
  const leaveTimer = timer.mock.calls.filter(([, delay]) => delay === 180).at(-1)![0] as () => void;
  act(() => { close(); open(); });
  act(() => { fireEvent.keyDown(screen.getByRole('button', { name: 'Timer child' }), { key: 'ArrowDown' }); });
  act(() => { leaveTimer(); });
  expect.soft(document.querySelector('[aria-label="Preview: Timer child"]')?.hasAttribute('data-nc-link-preview')).toBe(true);
});

for (const reduced of [false, true]) it(`initializes preparing DOM safely before child layout focus (${reduced ? 'reduced' : 'normal'})`, async () => {
  await page.viewport(1200, 900); await commands.emulateReducedMotion(reduced);
  vi.mocked(readSurfaceFrame).mockReturnValue(null);
  const focus = vi.fn(); const initial: unknown[] = [];
  function FocusProbe() {
    const input = useRef<HTMLInputElement>(null);
    useLayoutEffect(() => {
      const card = input.current!.closest<HTMLElement>('[data-nc-preview-owner]')!;
      const slot = card.closest<HTMLElement>('[data-nc-preview-slot]')!;
      initial.push([card.inert, slot.inert, card.getAttribute('aria-hidden'), slot.getAttribute('aria-hidden'), getComputedStyle(card.parentElement!).clipPath]);
      input.current!.focus();
    }, []);
    return <input ref={input} aria-label="Preparing input" onFocus={focus} />;
  }
  render(<HoverPreview title="Autofocus" trigger={activate => <button onClick={activate}>Autofocus</button>}>
    <FocusProbe />
  </HoverPreview>);
  const trigger = screen.getByRole('button', { name: 'Autofocus' });
  act(() => { trigger.focus(); fireEvent.click(trigger); });
  expect(focus).not.toHaveBeenCalled();
  expect(initial).toEqual([[true, true, 'true', 'true', 'inset(50%)']]);
  const card = document.querySelector<HTMLElement>('[aria-label="Preview: Autofocus"]')!;
  expect(card.inert).toBe(true); expect(card.getAttribute('aria-hidden')).toBe('true');
  expect(card.hasAttribute('data-nc-link-preview')).toBe(false);
});

for (const reduced of [false, true]) it(`resumes nested source sampling after ancestor measurement suspension (${reduced ? 'reduced' : 'normal'})`, async () => {
  await page.viewport(1800, 1000); await commands.emulateReducedMotion(reduced);
  let blocked = false;
  const { readSurfaceFrame: realFrame } = await vi.importActual<typeof import('../motion/geometry.ts')>('../motion/geometry.ts');
  vi.mocked(readSurfaceFrame).mockImplementation(element => blocked && element?.querySelector(':scope > [aria-label="Preview: Source parent"]') ? null : realFrame(element));
  function Fixture({ gap }: { gap: number }) {
    return <div style={{ padding: 80 }}><HoverPreview title="Source parent" trigger={activate => <button onClick={activate}>Source parent</button>}>
      <div style={{ height: gap }} /><HoverPreview title="Source child" trigger={activate => <button onClick={activate}>Source child</button>}><p>Child content</p></HoverPreview>
    </HoverPreview><div data-testid="source-scroll-input" /></div>;
  }
  const fixture = render(<Fixture gap={0} />);
  act(() => { fireEvent.click(screen.getByRole('button', { name: 'Source parent' })); });
  act(() => { fireEvent.click(screen.getByRole('button', { name: 'Source child' })); });
  const parent = screen.getByRole('dialog', { name: 'Preview: Source parent' });
  const child = screen.getByRole('dialog', { name: 'Preview: Source child' });
  for (const card of [parent, child]) card.getAnimations().forEach(animation => { animation.pause(); animation.currentTime = 80; });
  await frames(4); blocked = true;
  // The live proof must fence source ownership before a queued pending notice.
  // Direct visual sampling cannot rely on an inverse null to reveal this loss.
  expect(readPort(parent)).toBeNull(); expect(readPort(child)).toBeNull();
  act(() => { fireEvent.scroll(screen.getByTestId('source-scroll-input')); }); await frames(4);
  fixture.rerender(<Fixture gap={40} />); await frames(4);
  blocked = false;
  act(() => { fireEvent.scroll(screen.getByTestId('source-scroll-input')); }); await frames(8);
  expect(document.querySelectorAll('[data-nc-link-preview]')).toHaveLength(2);
  for (const card of [parent, child]) card.getAnimations().forEach(animation => animation.finish());
  await frames(2); await commands.emulateReducedMotion(false);
  const source = screen.getByRole('button', { name: 'Source child' }).parentElement!.getBoundingClientRect();
  const point = { x: source.left + source.width / 2, y: source.top + source.height / 2 };
  act(() => { fireEvent.keyDown(document, { key: 'Escape' }); });
  const exit = child.getAnimations()[0]; expect(exit).toBeDefined(); exit.pause(); exit.currentTime = Number(exit.effect!.getTiming().duration);
  const actual = child.parentElement!.getBoundingClientRect();
  const matrix = new DOMMatrix(getComputedStyle(child).transform);
  // Independent fixed point: the current unanimated anchor is fixed by native
  // lift paint, apart from the shared camera's documented 0.56px final sink.
  const painted = new DOMPoint(point.x - actual.left, point.y - actual.top).matrixTransform(matrix);
  expect(actual.left + painted.x).toBeCloseTo(point.x, 3);
  expect(actual.top + painted.y).toBeCloseTo(point.y + 0.56, 3);
});
