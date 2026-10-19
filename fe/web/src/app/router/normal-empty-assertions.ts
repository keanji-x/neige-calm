// Shared schema checks, independent of screenshot file names and browser staging.
import { expect } from 'vitest';
import { page } from 'vitest/browser';

export function assertNormalEmptySurface(expectedTitles: readonly string[], styles: Readonly<Record<string, string>>) {
  for (const title of expectedTitles) {
    const headings = page.getByRole('heading',{name:title,exact:true}).elements();
    expect(headings.length, title).toBeGreaterThan(0);
    for (const heading of headings) {
      const root = heading.parentElement!.parentElement!;
      expect(root.classList.contains(styles.empty)).toBe(true);
      expect(root, 'must use shared schema').not.toBeNull();
      const compact = root.classList.contains(styles.compact);
      const panel = root.classList.contains(styles.panel);
      const typography = getComputedStyle(heading);
      expect(typography.fontSize).toBe(compact ? '14px' : panel ? '16px' : '20px');
      expect(typography.fontWeight).toBe('500');
      expect(typography.lineHeight).toBe(compact ? '20px' : panel ? '26.4px' : '24px');
      expect(getComputedStyle(root).rowGap).toBe(compact ? '4px' : '8px');
      expect(getComputedStyle(heading.parentElement!).columnGap).toBe(compact ? '6px' : '8px');
      const description = root.querySelector('p')!;
      expect(description.textContent.length).toBeGreaterThan(0);
      expect(getComputedStyle(description).fontSize).toBe(compact ? '12px' : '14px');
      expect(getComputedStyle(description).lineHeight).toBe(compact ? '18px' : '23.1px');
      expect(root.querySelector('[aria-hidden="true"] svg')).not.toBeNull();
      expect(root.querySelector('button,a,input,textarea,[tabindex]')).toBeNull();
    }
  }
}
