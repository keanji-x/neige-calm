// Smoke-test fixture for `neige-calm/no-raw-primitive-role`.
//
// This file intentionally violates the rule — raw `role="dialog"`,
// `role="menu"`, and `role="menuitem"` on plain JSX elements outside
// the `web/src/ui/` primitive layer. The companion vitest spec
// (`../no-raw-primitive-role.test.ts`) runs ESLint programmatically
// against this file and asserts that all three violations are
// reported with the expected per-role message ids. The fixture is
// excluded from the regular lint pass via the `ignores` entry in
// `eslint.config.js`.

export function BadDialog() {
  return (
    <div role="dialog" aria-modal="true" aria-label="Bad">
      <p>Should use the Dialog primitive.</p>
    </div>
  );
}

export function BadMenu() {
  return (
    <ul role="menu" aria-label="Bad">
      <li>
        <button type="button" role="menuitem">
          Should use the Menu primitive
        </button>
      </li>
    </ul>
  );
}

// Brace-wrapped string literal — equivalent to the bare form and
// flagged identically. Pins the regression that `role={"dialog"}`
// is not a trivial bypass.
export function BadBraceDialog() {
  return <section role={'dialog'}>brace-wrapped</section>;
}

// Allowed — non-flagged ARIA roles outside the primitive layer are
// fine. The rule narrowly targets dialog / menu / menuitem.
export function OkOtherRole() {
  return <div role="region" aria-label="Region">ok</div>;
}

// Allowed — dynamic role expression. The linter cannot statically
// prove the value, so we deliberately let it through (false positives
// would be worse than the rare miss).
export function OkDynamicRole({ kind }: { kind: 'dialog' | 'region' }) {
  return <div role={kind}>dynamic</div>;
}
