/**
 * Owner manifest for INV-DUP-001..010. P8 may reference this file; it must not
 * duplicate these ownership decisions.
 *
 * `unique-symbol` means the named public implementation may only be declared
 * at canonicalPath and consumers must import it from there. The markdown pair
 * additionally owns disjoint third-party import families so each fence has an
 * independent mutation signal while sharing core/markdown/public.ts.
 */
/**
 * @typedef {Readonly<{
 *   id: string,
 *   type: 'unique-symbol' | 'import-fence',
 *   canonicalPath: string,
 *   symbols: readonly string[],
 *   packages?: readonly string[],
 *   mergeObligations?: readonly string[],
 * }>} DuplicationEntry
 */

/** @param {DuplicationEntry} entry @returns {DuplicationEntry} */
function freezeEntry(entry) {
  return Object.freeze({
    ...entry,
    symbols: Object.freeze(entry.symbols),
    ...(entry.packages ? { packages: Object.freeze(entry.packages) } : {}),
    ...(entry.mergeObligations ? { mergeObligations: Object.freeze(entry.mergeObligations) } : {}),
  });
}

export const duplicationManifest = Object.freeze(/** @type {DuplicationEntry[]} */ ([
  { id: 'INV-DUP-001', type: 'unique-symbol', canonicalPath: 'web/src/ui/schema-form/public.tsx', symbols: ['SchemaForm'] },
  { id: 'INV-DUP-002', type: 'unique-symbol', canonicalPath: 'web/src/ui/calm-select/public.tsx', symbols: ['CalmSelect'] },
  { id: 'INV-DUP-003', type: 'unique-symbol', canonicalPath: 'web/src/ui/schema-form/fields/public.tsx', symbols: ['FormField'] },
  { id: 'INV-DUP-004', type: 'import-fence', canonicalPath: 'core/markdown/public.ts', symbols: ['parse', 'sanitizeAstPolicy'], packages: ['react-markdown', 'remark-*', 'rehype-*', 'unified'], mergeObligations: ['SpecConversation 的 agent 气泡只有 remarkGfm，没有 urlTransform、没有 ReportLink；与前三者的差异合并时必须显式决策而非默认对齐。'] },
  { id: 'INV-DUP-005', type: 'import-fence', canonicalPath: 'core/markdown/public.ts', symbols: ['extractOutline'], packages: ['mdast-util-*', 'micromark*'], mergeObligations: ['必须保留 `<blockId>-h<n>` 与 `md-h-` 两种 id 前缀方案各自的锚点稳定性；报告大纲只要两级、文件预览要四级。'] },
  { id: 'INV-DUP-006', type: 'unique-symbol', canonicalPath: 'web/src/features/cove/palette.ts', symbols: ['COVE_PALETTE'], mergeObligations: ['COVE_PALETTE 必须保持唯一 canonical 符号；当前唯一消费者是 Sidebar，创建 cove 时用 Math.random 随机取色并把 color 发给内核。'] },
  { id: 'INV-DUP-007', type: 'unique-symbol', canonicalPath: 'web/src/app/theme/host-rgb.ts', symbols: ['readHostThemeRgb'] },
  { id: 'INV-DUP-008', type: 'unique-symbol', canonicalPath: 'web/src/ui/editable-title/public.tsx', symbols: ['EditableTitle'], mergeObligations: ['Cove 版有 #288 的合成 click 抑制器；合并时必须把抑制器带进统一实现，而不是删掉。'] },
  { id: 'INV-DUP-009', type: 'unique-symbol', canonicalPath: 'web/src/features/wave/row/public.tsx', symbols: ['WaveRow'] },
  { id: 'INV-DUP-010', type: 'unique-symbol', canonicalPath: 'web/src/ui/confirm-dialog/copy.ts', symbols: ['DELETE_WAVE_COPY'] },
]).map(freezeEntry));
