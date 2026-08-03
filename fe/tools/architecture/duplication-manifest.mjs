/**
 * Owner manifest for INV-DUP-001..010. P8 may reference this file; it must not
 * duplicate these ownership decisions.
 *
 * `unique-symbol` means the named public implementation may only be declared
 * at canonicalPath and consumers must import it from there. The markdown pair
 * additionally owns disjoint third-party import families so each fence has an
 * independent mutation signal while sharing core/markdown/public.ts.
 */
export const duplicationManifest = Object.freeze([
  { id: 'INV-DUP-001', type: 'unique-symbol', canonicalPath: 'web/src/ui/schema-form/public.tsx', symbols: ['SchemaForm'] },
  { id: 'INV-DUP-002', type: 'unique-symbol', canonicalPath: 'web/src/ui/calm-select/public.tsx', symbols: ['CalmSelect'] },
  { id: 'INV-DUP-003', type: 'unique-symbol', canonicalPath: 'web/src/ui/schema-form/fields/public.tsx', symbols: ['FormField'] },
  { id: 'INV-DUP-004', type: 'import-fence', canonicalPath: 'core/markdown/public.ts', symbols: ['parse', 'sanitizeAstPolicy'], packages: ['react-markdown', 'remark-*', 'rehype-*', 'unified'] },
  { id: 'INV-DUP-005', type: 'import-fence', canonicalPath: 'core/markdown/public.ts', symbols: ['extractOutline'], packages: ['mdast-util-*', 'micromark*'] },
  { id: 'INV-DUP-006', type: 'unique-symbol', canonicalPath: 'web/src/features/cove/palette.ts', symbols: ['COVE_PALETTE'] },
  { id: 'INV-DUP-007', type: 'unique-symbol', canonicalPath: 'web/src/app/theme/host-rgb.ts', symbols: ['readHostThemeRgb'] },
  { id: 'INV-DUP-008', type: 'unique-symbol', canonicalPath: 'web/src/ui/editable-title/public.tsx', symbols: ['EditableTitle'] },
  { id: 'INV-DUP-009', type: 'unique-symbol', canonicalPath: 'web/src/features/wave/row/public.tsx', symbols: ['WaveRow'] },
  { id: 'INV-DUP-010', type: 'unique-symbol', canonicalPath: 'web/src/ui/confirm-dialog/copy.ts', symbols: ['DELETE_WAVE_COPY'] },
]);
