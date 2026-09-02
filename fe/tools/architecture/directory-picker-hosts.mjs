// CAP-WAVEWORKSPACE-003's fail-closed sweep: every surface that can put a
// directory picker on screen must be registered, with how it hosts it.
//
// ## Why a sweep and not a test
//
// The invariant is a universal negative — "no surface may render the browser
// inline in the page" — and a universal negative cannot be carried by a test
// that renders one component. That test proves the registered surface behaves;
// it says nothing about the surface someone adds next week, which is exactly
// the one that will get it wrong.
//
// The hazard is concrete and still live in the tree. `DirectoryField` picks its
// behaviour by asking `useDialogView()` whether a dialog is above it:
//
//   * inside a dialog it pushes a child view (the modal is the outer dialog);
//   * outside one it falls back to rendering `DirectoryBrowser` **inline**.
//
// That fallback is not a defect in `DirectoryField` — it is a documented escape
// for hosts that are not dialogs — but it means "did the picker open as a
// modal?" is a property of the *call site*, decided silently, with no type and
// no runtime error to announce it. #1211 walked straight into it: moving the
// new-wave form from a dialog onto a route flipped that branch, and the picker
// became a file list unrolled under a chip with no focus trap, no Escape and no
// click-outside. Every suite stayed green, because the control was still
// `DirectoryField` and every assertion was about `DirectoryField`.
//
// So the check is on the set of call sites, and it fails closed: a file that
// renders either component and is not registered below is an error, and the
// registration has to say which host it is. Adding a picker therefore forces
// the author to answer the question that was previously answered by accident.
//
// ## What this does and does not prove
//
// It proves the set is *known*. It does not prove each entry's claim — that
// `new-card` really pushes into a surrounding dialog, and that `new-wave`
// really opens its own modal, are behavioural facts, and each is pinned by the
// `authoritative_test` its oracle row names (CAP-WAVEWORKSPACE-006 and -003
// respectively). The two halves are deliberate: this file cannot execute React,
// and those tests cannot see a call site nobody wrote yet.

import { readdirSync, readFileSync } from 'node:fs';
import { extname, resolve } from 'node:path';

/**
 * How a registered surface puts the picker on screen.
 *
 *   `pushes-into-host-dialog` — the surface is itself inside a `Dialog`, so
 *     `DirectoryField` takes its `useDialogView()` branch and the picker
 *     replaces the host dialog's body. Nesting a second dialog here would
 *     fight the outer one's focus trap (CAP-WAVEWORKSPACE-006).
 *
 *   `owns-its-modal` — the surface is not inside a dialog, so it must mount a
 *     `Dialog` of its own around `DirectoryBrowser`. Using `DirectoryField`
 *     here would silently take the inline fallback (CAP-WAVEWORKSPACE-003).
 */
export const DIRECTORY_PICKER_HOSTS = Object.freeze({
  'web/src/ui/schema-form/fields/DirectoryField/public.tsx': 'component',
  'web/src/features/wave/new-card/public.tsx': 'pushes-into-host-dialog',
  'web/src/features/cove/new-wave/public.tsx': 'owns-its-modal',
});

/** The two components whose presence makes a file a picker host. */
const RENDERS = [/<DirectoryField[\s/>]/, /<DirectoryBrowser[\s/>]/];

/**
 * Every candidate source file under `root`, as paths relative to it.
 *
 * @param {string} root
 * @returns {string[]}
 */
function sourceFiles(root) {
  return readdirSync(root, { recursive: true })
    .map(String)
    .filter((entry) => ['.tsx', '.jsx'].includes(extname(entry)))
    // Tests may render either component freely: they are the things that prove
    // the hosts behave, and a test is not a surface a user can reach.
    .filter((entry) => !/\.(test|spec|browser\.test|contract\.test)\./.test(entry));
}

export function checkDirectoryPickerHosts(webSrc = 'web/src') {
  const root = resolve(webSrc);
  const problems = [];
  const seen = new Set();
  for (const entry of sourceFiles(root)) {
    const path = `web/src/${entry.split('\\').join('/')}`;
    const contents = readFileSync(resolve(root, entry), 'utf8');
    if (!RENDERS.some((pattern) => pattern.test(contents))) continue;
    seen.add(path);
    if (!(path in DIRECTORY_PICKER_HOSTS)) {
      problems.push(`${path} renders a directory picker but is not registered in `
        + 'tools/architecture/directory-picker-hosts.mjs — declare whether it pushes into a '
        + 'host dialog or owns its own modal (CAP-WAVEWORKSPACE-003 / -006)');
    }
  }
  for (const path of Object.keys(DIRECTORY_PICKER_HOSTS)) {
    if (!seen.has(path)) {
      problems.push(`${path} is registered as a directory picker host but renders neither `
        + 'DirectoryField nor DirectoryBrowser — drop the stale registration');
    }
  }
  return problems.length ? `directory-picker-hosts:\n  ${problems.join('\n  ')}` : '';
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const error = checkDirectoryPickerHosts(process.argv[2]);
  if (error) {
    console.error(error);
    process.exitCode = 1;
  }
}
