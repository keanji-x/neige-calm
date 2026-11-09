import { cleanup, render } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import '../../styles/entry.css';
import { Reply } from '../../features/chat/thread/reply.tsx';
import { ReportDocument } from '../../features/report/document/public.tsx';

afterEach(cleanup);

it('renders formulas in both conversation and report at a narrow width', () => {
  const source = 'Inline $x^2$.\n\n$$\n\\frac{1}{2}\n$$';
  const { container } = render(<div style={{ width: 320 }}>
    <Reply text={source} imageFiles={null} />
    <ReportDocument report={{ summary: '', body: source, blocks: null }} empty={<></>} />
  </div>);
  const formulas = container.querySelectorAll('math');
  expect(formulas).toHaveLength(4);
  for (const formula of formulas) {
    expect(formula.parentElement?.parentElement?.getBoundingClientRect().width).toBeGreaterThan(0);
  }
  expect(container.firstElementChild?.scrollWidth).toBeLessThanOrEqual(320);
});


it('scrolls a standalone formula with trailing horizontal whitespace inside each renderer', () => {
  const source = `$$${Array.from({ length: 40 }, () => 'x^2').join('+')}$$  \t`;
  const { container } = render(<div style={{ width: 240 }}>
    <Reply text={source} imageFiles={null} />
    <ReportDocument report={{ summary: '', body: source, blocks: null }} empty={<></>} />
  </div>);
  const displays = container.querySelectorAll<HTMLElement>('[data-nc-math-display]');
  expect(displays).toHaveLength(2);
  for (const display of displays) {
    expect(display.scrollWidth).toBeGreaterThan(display.clientWidth);
    display.scrollLeft = 30;
    expect(display.scrollLeft).toBe(30);
  }
  expect(container.firstElementChild!.scrollWidth).toBeLessThanOrEqual(240);
});
