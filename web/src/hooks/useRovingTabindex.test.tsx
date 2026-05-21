// Unit tests for `useRovingTabindex`.
//
// We render a tiny harness — a vertical list of buttons that consume the
// hook — and drive it with `@testing-library/user-event` so we exercise the
// real key-event path React sees. Each test asserts both the focus side-
// effect (the right button is `:focus`) and the structural side-effect
// (only the active button has `tabIndex=0`).
//
// The pure helpers (`findTypeaheadMatch`, `normalizeForTypeahead`) get
// direct unit tests too — they're small enough that the integration tests
// would mask off-by-ones.

import { describe, it, expect, vi } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
// `vi` is referenced by the `onActivate` / `onEscape` mocks above; the
// typeahead block uses native setTimeout instead of fake timers. Kept
// imported for the upper-block mocks.
import {
  findTypeaheadMatch,
  normalizeForTypeahead,
  useRovingTabindex,
} from './useRovingTabindex';

interface MenuProps {
  items: string[];
  onActivate?: (i: number) => void;
  onEscape?: () => void;
  loop?: boolean;
  typeaheadTimeoutMs?: number;
  withGetLabel?: boolean;
}

// Minimal harness component: a vertical list of buttons wired to the hook.
function Menu({
  items,
  onActivate,
  onEscape,
  loop,
  typeaheadTimeoutMs,
  withGetLabel = true,
}: MenuProps) {
  const { activeIndex, getItemProps } = useRovingTabindex<HTMLButtonElement>({
    itemCount: items.length,
    onActivate,
    onEscape,
    loop,
    getLabel: withGetLabel ? (i) => items[i] ?? '' : undefined,
    typeaheadTimeoutMs,
  });
  return (
    <ul role="menu" data-testid="menu" aria-label={`active=${activeIndex}`}>
      {items.map((label, i) => {
        const { ref, tabIndex, onKeyDown } = getItemProps(i);
        return (
          <li key={label} role="none">
            <button
              ref={ref}
              role="menuitem"
              tabIndex={tabIndex}
              onKeyDown={onKeyDown}
            >
              {label}
            </button>
          </li>
        );
      })}
    </ul>
  );
}

describe('findTypeaheadMatch (pure)', () => {
  const labels = ['Apple', 'Banana', 'Cherry', 'apricot', 'blueberry'];

  it('matches a single letter starting after the current index', () => {
    // Single-letter buffer cycles past the current item.
    expect(findTypeaheadMatch(labels, 'a', 0)).toBe(3); // 'apricot' (skip 'Apple' at startFrom)
    expect(findTypeaheadMatch(labels, 'a', 3)).toBe(0); // wrap to 'Apple'
  });

  it('matches case-insensitively', () => {
    expect(findTypeaheadMatch(labels, 'B', 0)).toBe(1); // 'Banana'
    expect(findTypeaheadMatch(labels, 'BAN', 0)).toBe(1);
  });

  it('multi-letter buffer keeps the current item if it still matches', () => {
    // Two-letter buffer includes the start index. So if start=3 ('apricot')
    // and buffer='ap', the match should be 3, not the next 'a'.
    expect(findTypeaheadMatch(labels, 'ap', 3)).toBe(3);
  });

  it('returns -1 when nothing matches', () => {
    expect(findTypeaheadMatch(labels, 'zz', 0)).toBe(-1);
    expect(findTypeaheadMatch([], 'a', 0)).toBe(-1);
    expect(findTypeaheadMatch(labels, '', 0)).toBe(-1);
  });

  it('normalize trims and lowercases', () => {
    expect(normalizeForTypeahead('  AbC  ')).toBe('abc');
  });
});

describe('useRovingTabindex — arrow / Home / End', () => {
  const items = ['One', 'Two', 'Three'];

  it('initial render: only first item has tabIndex=0', () => {
    render(<Menu items={items} />);
    const buttons = screen.getAllByRole('menuitem');
    expect(buttons[0]).toHaveAttribute('tabindex', '0');
    expect(buttons[1]).toHaveAttribute('tabindex', '-1');
    expect(buttons[2]).toHaveAttribute('tabindex', '-1');
  });

  it('ArrowDown moves focus forward; wraps at the end', async () => {
    const user = userEvent.setup();
    render(<Menu items={items} />);
    const buttons = screen.getAllByRole('menuitem');
    // Focus the first to start the cycle — in production AddPanel does
    // this via the ref-mount microtask focus. Tests can do it directly.
    buttons[0].focus();
    expect(buttons[0]).toHaveFocus();
    await user.keyboard('{ArrowDown}');
    expect(buttons[1]).toHaveFocus();
    expect(buttons[1]).toHaveAttribute('tabindex', '0');
    expect(buttons[0]).toHaveAttribute('tabindex', '-1');
    await user.keyboard('{ArrowDown}');
    expect(buttons[2]).toHaveFocus();
    // Wrap: ArrowDown from last → first.
    await user.keyboard('{ArrowDown}');
    expect(buttons[0]).toHaveFocus();
  });

  it('ArrowUp moves focus backward; wraps at the start', async () => {
    const user = userEvent.setup();
    render(<Menu items={items} />);
    const buttons = screen.getAllByRole('menuitem');
    buttons[0].focus();
    await user.keyboard('{ArrowUp}');
    expect(buttons[2]).toHaveFocus(); // wraps
    await user.keyboard('{ArrowUp}');
    expect(buttons[1]).toHaveFocus();
  });

  it('loop=false clamps at the boundaries instead of wrapping', async () => {
    const user = userEvent.setup();
    render(<Menu items={items} loop={false} />);
    const buttons = screen.getAllByRole('menuitem');
    buttons[0].focus();
    await user.keyboard('{ArrowUp}');
    expect(buttons[0]).toHaveFocus(); // clamped, not wrapped
    await user.keyboard('{ArrowDown}');
    await user.keyboard('{ArrowDown}');
    expect(buttons[2]).toHaveFocus();
    await user.keyboard('{ArrowDown}');
    expect(buttons[2]).toHaveFocus(); // clamped
  });

  it('Home jumps to the first item, End jumps to the last', async () => {
    const user = userEvent.setup();
    render(<Menu items={items} />);
    const buttons = screen.getAllByRole('menuitem');
    buttons[0].focus();
    await user.keyboard('{End}');
    expect(buttons[2]).toHaveFocus();
    await user.keyboard('{Home}');
    expect(buttons[0]).toHaveFocus();
  });
});

describe('useRovingTabindex — Enter / Space / Escape', () => {
  it('Enter calls onActivate with the active index', async () => {
    const user = userEvent.setup();
    const onActivate = vi.fn();
    render(<Menu items={['a', 'b', 'c']} onActivate={onActivate} />);
    const buttons = screen.getAllByRole('menuitem');
    buttons[0].focus();
    await user.keyboard('{ArrowDown}');
    await user.keyboard('{Enter}');
    expect(onActivate).toHaveBeenCalledTimes(1);
    expect(onActivate).toHaveBeenCalledWith(1);
  });

  it('Space calls onActivate when the typeahead buffer is empty', async () => {
    const user = userEvent.setup();
    const onActivate = vi.fn();
    render(<Menu items={['a', 'b', 'c']} onActivate={onActivate} />);
    const buttons = screen.getAllByRole('menuitem');
    buttons[0].focus();
    await user.keyboard(' ');
    expect(onActivate).toHaveBeenCalledWith(0);
  });

  it('Escape calls onEscape', async () => {
    const user = userEvent.setup();
    const onEscape = vi.fn();
    render(<Menu items={['a', 'b']} onEscape={onEscape} />);
    const buttons = screen.getAllByRole('menuitem');
    buttons[0].focus();
    await user.keyboard('{Escape}');
    expect(onEscape).toHaveBeenCalledTimes(1);
  });
});

// Typeahead is tricky to pair with `userEvent` + fake timers — the
// userEvent async loop hangs when both are active. We drive the
// keystrokes through `fireEvent.keyDown` directly here (the hook listens
// on `onKeyDown` so this is the same code path) and use real timers,
// plus a very short typeaheadTimeoutMs in tests so we can assert the
// idle-reset behavior without sleeping.
describe('useRovingTabindex — typeahead', () => {
  function pressKey(target: HTMLElement, key: string): void {
    fireEvent.keyDown(target, { key });
  }

  it('jumps to the first item matching the typed prefix', () => {
    render(<Menu items={['Apple', 'Banana', 'Cherry']} />);
    const buttons = screen.getAllByRole('menuitem');
    buttons[0].focus();
    pressKey(buttons[0], 'c');
    expect(buttons[2]).toHaveFocus();
  });

  it('cycles through items sharing the same first letter on repeat keypress', async () => {
    render(
      <Menu items={['apple', 'apricot', 'banana']} typeaheadTimeoutMs={20} />,
    );
    const buttons = screen.getAllByRole('menuitem');
    buttons[0].focus();
    // First 'a': cycles past 'apple' → 'apricot'.
    pressKey(buttons[0], 'a');
    expect(buttons[1]).toHaveFocus();
    // Let the buffer time out, then another 'a' wraps to 'apple'.
    await new Promise((r) => setTimeout(r, 40));
    pressKey(buttons[1], 'a');
    expect(buttons[0]).toHaveFocus();
  });

  it('multi-letter buffer narrows the match', () => {
    render(<Menu items={['apple', 'apricot', 'avocado']} />);
    const buttons = screen.getAllByRole('menuitem');
    buttons[0].focus();
    pressKey(buttons[0], 'a');
    pressKey(buttons[0], 'p');
    // 'ap' matches both 'apple' and 'apricot' — first hit from current
    // (multi-letter buffer includes start). startFrom=0 with 'a' lands
    // on 'apricot' (idx 1); then 'p' extends buffer to 'ap' and from
    // current index 1, includeStart=true → still 'apricot' (idx 1).
    expect(buttons[1]).toHaveFocus();
    pressKey(buttons[1], 'r');
    expect(buttons[1]).toHaveFocus(); // 'apr' still matches apricot
  });

  it('buffer clears after the idle timeout', async () => {
    render(
      <Menu
        items={['apple', 'apricot', 'banana']}
        typeaheadTimeoutMs={20}
      />,
    );
    const buttons = screen.getAllByRole('menuitem');
    buttons[0].focus();
    pressKey(buttons[0], 'a');
    pressKey(buttons[1], 'p'); // narrows; stays on apricot
    expect(buttons[1]).toHaveFocus();
    // Wait past the idle timeout.
    await new Promise((r) => setTimeout(r, 40));
    // Fresh 'b' jumps to banana — proves the buffer reset.
    pressKey(buttons[1], 'b');
    expect(buttons[2]).toHaveFocus();
  });

  it('typeahead is disabled when getLabel is not supplied', () => {
    render(<Menu items={['apple', 'banana']} withGetLabel={false} />);
    const buttons = screen.getAllByRole('menuitem');
    buttons[0].focus();
    pressKey(buttons[0], 'b'); // no typeahead — focus stays put
    expect(buttons[0]).toHaveFocus();
  });
});

