// Smoke-test fixture for `neige-calm/no-react-state-hook-members`.
//
// These calls intentionally bypass `src/shared/state.ts` via namespace and
// default React imports. The companion vitest spec asserts that all three
// calls are reported.

import * as React from 'react';
import ReactDefault from 'react';

export function badNamespaceState() {
  return React.useState(0);
}

export function badNamespaceReducer() {
  return React.useReducer((state: number) => state + 1, 0);
}

export function badDefaultAlias() {
  return ReactDefault.useState('x');
}
