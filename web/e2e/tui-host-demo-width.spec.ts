// Regression guard for issue #1152: the `tui-host-demo` fixture must paint
// `TUI_HOST_DEMO_READY` in full at every terminal width the card can hand it.
//
// CI measured the card at exactly 60 columns and `clip(footer, cols)` chopped
// the last character off the marker, so `tui-host-demo.spec.ts` polled for it
// until its 15s timeout. Whether it landed on 60 or >=61 depended on font
// metrics / layout timing, which is what made it a flake rather than a hard red.
//
// This spec deliberately owns no logic: the decisive check is a Python harness
// that drives the REAL fixture through a REAL pty, because the failure only
// exists at the pty/winsize seam. Living here means it rides the existing
// `chromium` e2e job with no extra CI wiring, and it needs no browser, no
// calm-server and no dev stack.

import { execFileSync } from 'node:child_process';
import * as path from 'node:path';
import { fileURLToPath } from 'node:url';
import { test } from '@playwright/test';

const harness = path.resolve(
  fileURLToPath(import.meta.url),
  '../fixtures/tui-host-demo-width-check.py',
);

test('tui-host-demo paints the READY marker whole at narrow and wide widths', () => {
  // 60 = the width CI actually rendered and where the marker used to lose its
  // tail. 100 = a comfortable width, green before and after the fix, so a
  // failure here means the fixture broke outright rather than "narrow is hard".
  execFileSync('python3', [harness, '60', '100'], { stdio: 'inherit' });
});
