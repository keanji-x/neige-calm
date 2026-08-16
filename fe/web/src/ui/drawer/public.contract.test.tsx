import { describe, expect, it } from 'vitest';

import { DrawerAction } from './public.tsx';

describe('DrawerAction type contract', () => {
  it('accepts elements and rejects font glyph children', () => {
    expect(<DrawerAction label="Reset conversation" onClick={() => undefined}>
      <svg aria-hidden="true" />
    </DrawerAction>).toBeTruthy();
    // @ts-expect-error -- an icon action must receive an element, never a font glyph.
    const rejected = <DrawerAction label="Reset conversation" onClick={() => undefined}>↺</DrawerAction>;
    expect(rejected).toBeTruthy();
  });
});
