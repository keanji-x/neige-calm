import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';

const css = readFileSync(resolve(import.meta.dirname, 'dialog.css'), 'utf8');

describe('busy label visibility contract', () => {
  it('depends on the labelled button state, not a particular ancestor', () => {
    expect(css).toContain("[data-nc-action]:not([data-nc-state='busy']) > .confirm-dialog-label > :last-child");
    expect(css).not.toContain(".confirm-dialog-actions [data-nc-action]:not([data-nc-state='busy'])");
  });
});
