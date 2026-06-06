import { describe, expect, expectTypeOf, it } from 'vitest';
import {
  isKernelOverlayKind,
  kernelOverlayKinds,
  useTypedCardOverlay,
  type KernelOverlayKind,
  type OverlayKindPayloadRegistry,
  type StatusOverlayPayload,
} from '../overlayRegistry';

describe('overlayRegistry', () => {
  it('lists the seven kernel overlay kinds', () => {
    const expected = [
      'status',
      'progress',
      'eta',
      'now',
      'layout',
      'file-viewer-nav',
      'any_card_needs_input',
    ] as const satisfies readonly (keyof OverlayKindPayloadRegistry)[];

    expect(kernelOverlayKinds).toHaveLength(7);
    expect(kernelOverlayKinds).toEqual(expected);
    expectTypeOf<KernelOverlayKind>().toEqualTypeOf<
      keyof OverlayKindPayloadRegistry
    >();
  });

  it('narrows kernel overlay kind strings', () => {
    expect(isKernelOverlayKind('status')).toBe(true);
    expect(isKernelOverlayKind('plugin-foo')).toBe(false);
  });

  it('narrows typed card overlays by kind', () => {
    expectTypeOf<
      ReturnType<typeof useTypedCardOverlay<'status'>>
    >().toEqualTypeOf<StatusOverlayPayload | null>();
    // type-only; hook never executed
    expectTypeOf<
      ReturnType<typeof useTypedCardOverlay<'plugin-foo'>>
    >().toEqualTypeOf<unknown>();
  });
});
