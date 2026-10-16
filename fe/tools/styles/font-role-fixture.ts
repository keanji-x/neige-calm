import type { TypographyToken } from '../../web/src/styles/public.ts';

type FontSnapshot = Readonly<Pick<CSSStyleDeclaration, 'fontFamily' | 'fontSize' | 'fontWeight' | 'lineHeight'>>;

/** Resolve an explicitly expected role in its consumer's ancestor context.
 * A distinct fixture font proves role inheritance rather than today's value.
 * Nothing imports runtime project code, discovers roles or pins Theme policy. */
export function withFontRoleProbe(expectedRole: TypographyToken, host: HTMLElement, check: (font: FontSnapshot) => void): void {
  const previous = host.style.getPropertyValue(expectedRole);
  const priority = host.style.getPropertyPriority(expectedRole);
  const reference = host.ownerDocument.createElement('span');
  reference.style.font = `var(${expectedRole})`;
  reference.style.position = 'absolute';
  reference.style.visibility = 'hidden';
  try {
    host.style.setProperty(expectedRole, '500 19px/27px monospace');
    host.append(reference);
    const computed = getComputedStyle(reference);
    check({ fontFamily: computed.fontFamily, fontSize: computed.fontSize,
      fontWeight: computed.fontWeight, lineHeight: computed.lineHeight });
  } finally {
    reference.remove();
    if (previous !== '') host.style.setProperty(expectedRole, previous, priority);
    else host.style.removeProperty(expectedRole);
  }
}
