// Custom ESLint rule: `no-raw-primitive-role`.
//
// Purpose
// -------
// Forbid raw `role="dialog"`, `role="menu"`, or `role="menuitem"` in JSX
// outside the `web/src/ui/` primitive layer. App code should compose the
// Neige primitives (`<Dialog>`, `<Menu>`, `<ConfirmDialog>`) — which own
// the role, focus, and keyboard contracts — rather than hand-rolling the
// ARIA role on a `<div>` / `<ul>` / `<button>`.
//
// This rule was promised in `web/src/ui/README.md` (Rules section) as
// part of issue #60 slice 1 ("App code does not query primitives by raw
// `role="…"`"). The primitives themselves legitimately implement the
// roles, so files under `web/src/ui/` are exempt.
//
// Stable message ids
// ------------------
// Per-role ids so call sites can pin against the exact violation in
// snapshot tests or future codemods:
//
//   - `noRawDialogRole`   — `role="dialog"` in app code
//   - `noRawMenuRole`     — `role="menu"` in app code
//   - `noRawMenuitemRole` — `role="menuitem"` in app code
//
// Detection
// ---------
// Match `JSXAttribute` nodes whose name is `role` and whose value is a
// string literal equal to one of the flagged roles. The match is purely
// syntactic — no type-checker required, and dynamic `role={expr}`
// expressions are deliberately not flagged (rare, and the linter can't
// statically prove the value).
//
// Fixable: NO. The correct fix is to replace the surrounding element
// with the corresponding primitive, which is not a safe mechanical
// transform (props, children, and focus wiring all differ).
//
// Exempt files
// ------------
// Files whose path contains `/web/src/ui/` (or `\web\src\ui\` on Windows)
// — the primitive layer. The rule's standalone test suite passes
// fixture file paths through this check too, so the exempt-path logic
// is exercised by both the test and the real-tree lint.

const FLAGGED_ROLES = {
  dialog: {
    messageId: 'noRawDialogRole',
    message:
      'Do not use `role="dialog"` directly. Compose the `<Dialog>` primitive from `web/src/ui/Dialog` instead — it owns the role, focus trap, Escape, and focus-restore contracts.',
  },
  menu: {
    messageId: 'noRawMenuRole',
    message:
      'Do not use `role="menu"` directly. Compose the `<Menu>` primitive from `web/src/ui/Menu` instead — it owns the role, roving tabindex, typeahead, and focus-restore contracts.',
  },
  menuitem: {
    messageId: 'noRawMenuitemRole',
    message:
      'Do not use `role="menuitem"` directly. Render menu items via the `<Menu>` primitive from `web/src/ui/Menu` instead of attaching the role to a bare `<button>`.',
  },
};

// Path substring used to detect the primitive layer. Matches both
// POSIX and Windows separators so the rule behaves the same on either
// platform (CI runs Linux; some contributors run Windows / WSL).
function isInsidePrimitiveLayer(filename) {
  if (!filename) return false;
  const normalized = filename.replace(/\\/g, '/');
  return normalized.includes('/web/src/ui/');
}

/** @type {import('eslint').Rule.RuleModule} */
const rule = {
  meta: {
    type: 'problem',
    docs: {
      description:
        'Disallow raw `role="dialog" | "menu" | "menuitem"` in JSX outside the `web/src/ui/` primitive layer; compose the Neige primitives instead.',
      recommended: true,
    },
    schema: [],
    messages: Object.fromEntries(
      Object.values(FLAGGED_ROLES).map((entry) => [entry.messageId, entry.message]),
    ),
  },

  create(context) {
    // `context.filename` is the ESLint 9 accessor; fall back to the
    // legacy getter for older callers (e.g. RuleTester invocations that
    // synthesize a context).
    const filename =
      typeof context.filename === 'string'
        ? context.filename
        : typeof context.getFilename === 'function'
          ? context.getFilename()
          : '';

    if (isInsidePrimitiveLayer(filename)) {
      // Exempt: primitives themselves legitimately implement these
      // roles. Returning an empty visitor object short-circuits the
      // rule for the whole file — cheaper than checking per-node.
      return {};
    }

    return {
      JSXAttribute(node) {
        // Only the `role` attribute is interesting. The attribute name
        // can be a plain `JSXIdentifier` (`role="…"`) or a
        // `JSXNamespacedName` (`xml:role="…"`); we only match the
        // unqualified `role` form because that's what carries ARIA
        // semantics in JSX.
        if (node.name?.type !== 'JSXIdentifier' || node.name.name !== 'role') {
          return;
        }

        // Only string-literal values are flagged. `role={expr}` is
        // allowed through — the linter cannot prove its value without
        // type information, and false positives on dynamic values
        // would be worse than the rare miss.
        const value = node.value;
        if (!value) return;

        let raw;
        if (value.type === 'Literal' && typeof value.value === 'string') {
          raw = value.value;
        } else if (
          value.type === 'JSXExpressionContainer' &&
          value.expression?.type === 'Literal' &&
          typeof value.expression.value === 'string'
        ) {
          // `role={"dialog"}` — same intent as `role="dialog"`, so
          // flag it too. Keeps the rule from being trivially bypassed
          // by wrapping the literal in braces.
          raw = value.expression.value;
        } else {
          return;
        }

        const entry = FLAGGED_ROLES[raw];
        if (!entry) return;

        context.report({
          node,
          messageId: entry.messageId,
        });
      },
    };
  },
};

module.exports = rule;
