// Built-in card entries. Each module exports a `CardEntry` constant;
// `registerBuiltins()` slots them into the registry once at app boot
// (called from `main.tsx`). Plugin cards register themselves later, via
// Slice F's iframe runtime, against the same `registerCard` API.

import { registerCard } from '../registry';
import { TerminalEntry } from './terminal';
import { DocEntry } from './doc';
import { GitEntry } from './git';
import { DiffEntry } from './diff';
import { PlanEntry } from './plan';
import { PluginIframeEntry } from '../plugin-iframe';

export { TerminalEntry, DocEntry, GitEntry, DiffEntry, PlanEntry, PluginIframeEntry };

let registered = false;

export function registerBuiltins(): void {
  // StrictMode double-mounts in dev would otherwise re-register and
  // double-warn; the Map dedupes by key anyway, but skipping the second
  // pass keeps the boot log clean.
  if (registered) return;
  registered = true;
  registerCard(TerminalEntry);
  registerCard(DocEntry);
  registerCard(GitEntry);
  registerCard(DiffEntry);
  registerCard(PlanEntry);
  // The plugin iframe entry is a built-in for now: it owns the `ui://`
  // kind namespace (the legacy `plugin:` form was deleted in M4). A
  // "plugin entry registers itself at runtime per-mount" model is part
  // of M5 full integration.
  registerCard(PluginIframeEntry);
}
