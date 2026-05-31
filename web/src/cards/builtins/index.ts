// Built-in card entries. Each module exports a `CardEntry` constant;
// `registerBuiltins()` slots them into the registry once at app boot
// (called from `main.tsx`). Plugin cards register themselves later, via
// Slice F's iframe runtime, against the same `registerCard` API.
//
// Today only `TerminalEntry` and `PluginIframeEntry` are live. The
// earlier `doc` / `git` / `diff` / `plan` builtins were removed in Wave 4
// — the kernel never produced those kinds, and the M3 plugin host
// (sandboxed `ui://` iframes) is the supported path for non-terminal
// cards going forward.

import { registerCard } from '../registry';
import { TerminalEntry } from './terminal';
import { ClaudeEntry, CodexEntry } from './codex';
import { WaveReportEntry } from './wave-report';
import { FileViewerEntry } from './file-viewer';
import { IframeEntry } from './iframe';
import { PluginIframeEntry } from '../plugin-iframe';

export { TerminalEntry, CodexEntry, ClaudeEntry, WaveReportEntry, FileViewerEntry, IframeEntry, PluginIframeEntry };

let registered = false;

export function registerBuiltins(): void {
  // StrictMode double-mounts in dev would otherwise re-register and
  // double-warn; the Map dedupes by key anyway, but skipping the second
  // pass keeps the boot log clean.
  if (registered) return;
  registered = true;
  registerCard(TerminalEntry);
  registerCard(CodexEntry);
  registerCard(ClaudeEntry);
  // Issue #229 PR B — wave-report card. Kernel-minted (one per wave),
  // kind = "wave-report". No `addPanel` entry — users cannot add
  // another one manually.
  registerCard(WaveReportEntry);
  registerCard(FileViewerEntry);
  registerCard(IframeEntry);
  // The plugin iframe entry is a built-in for now: it owns the `ui://`
  // kind namespace (the legacy `plugin:` form was deleted in M4). A
  // "plugin entry registers itself at runtime per-mount" model is part
  // of M5 full integration.
  registerCard(PluginIframeEntry);
}
