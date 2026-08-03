import { fireEvent, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup } from '@testing-library/react';
import { useRovingTabindex } from './public.ts';
import { useState } from '../state/public.ts';

function Roving({ onActivate = vi.fn(), onEscape = vi.fn() }: { onActivate?: (index: number) => void; onEscape?: () => void }) {
  const roving = useRovingTabindex<HTMLButtonElement>({ itemCount: 3, onActivate, onEscape, getLabel: (index) => ['Alpha', 'Beta', 'Gamma'][index] ?? '' });
  return <div data-testid="active" data-index={roving.activeIndex}>{['Alpha', 'Beta', 'Gamma'].map((label, index) => <button key={label} {...roving.getItemProps(index)}>{label}</button>)}</div>;
}

function DelayedItems() {
  const [show, setShow] = useState(false);
  const roving = useRovingTabindex<HTMLButtonElement>({ itemCount: 1 });
  return <>{show ? <button {...roving.getItemProps(0)}>Delayed item</button> : <button onClick={() => setShow(true)}>Show item</button>}</>;
}

const active = () => Number(screen.getByTestId('active').getAttribute('data-index'));
const key = (name: string) => fireEvent.keyDown(screen.getAllByRole('button')[active()], { key: name });
afterEach(cleanup);

describe('vertical roving keyboard behavior', () => {
  it('ArrowDown moves to the next item', () => { render(<Roving />); key('ArrowDown'); expect(active()).toBe(1); });
  it('ArrowUp wraps to the last item', () => { render(<Roving />); key('ArrowUp'); expect(active()).toBe(2); });
  it('Home moves to the first item', () => { render(<Roving />); key('ArrowDown'); key('Home'); expect(active()).toBe(0); });
  it('End moves to the last item', () => { render(<Roving />); key('End'); expect(active()).toBe(2); });
  it('Enter activates the active item', () => { const onActivate = vi.fn(); render(<Roving onActivate={onActivate}/>); key('Enter'); expect(onActivate).toHaveBeenCalledWith(0); });
  it('Escape invokes its callback', () => { const onEscape = vi.fn(); render(<Roving onEscape={onEscape}/>); key('Escape'); expect(onEscape).toHaveBeenCalledOnce(); });
  it('Space activates when the typeahead buffer is empty', () => { const onActivate = vi.fn(); render(<Roving onActivate={onActivate}/>); key(' '); expect(onActivate).toHaveBeenCalledWith(0); });
  it('Space extends a non-empty typeahead buffer without activation', () => { const onActivate = vi.fn(); render(<Roving onActivate={onActivate}/>); key('a'); key(' '); expect(onActivate).not.toHaveBeenCalled(); });
  it('focuses an active item that mounts after the active index is established', async () => {
    render(<DelayedItems/>); fireEvent.click(screen.getByRole('button', { name: 'Show item' }));
    await Promise.resolve();
    expect(document.activeElement).toBe(screen.getByRole('button', { name: 'Delayed item' }));
  });
  it.each(['ArrowLeft', 'ArrowRight'])('%s neither moves nor prevents default', (name) => {
    render(<Roving />);
    const result = key(name);
    expect(active()).toBe(0);
    expect(result).toBe(true);
  });
});
