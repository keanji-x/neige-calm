export function auditDataAttributes(code: string, file: string): string[];
export function auditCssImports(code: string, file: string): string[];
export const EXPECTED_LAYER_ORDER: readonly string[];
export const CSS_SOURCE_ENTRY_FORMS: readonly string[];
export const DATA_ATTRIBUTE_SOURCE_FORMS: readonly string[];
export function auditUnlayeredExceptions(css: string, file: string, order: readonly string[], entries: readonly { selector: string; property: string; expiry: string }[], today?: string): string[];
export function auditModuleLayer(css: string, file: string): string[];
export function auditStyleRepository(feRoot: string): string[];
