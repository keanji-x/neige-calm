/// <reference lib="dom" />

import { expect, it } from 'vitest';

it('uses a layout engine for rendered element geometry', () => {
  const element = document.createElement('div');
  element.style.height = '37px';
  element.style.width = '11px';
  document.body.append(element);

  expect(element.getBoundingClientRect().height).toBe(37);

  element.remove();
});
