import { LanguageDescription } from '@codemirror/language';
import { languages } from '@codemirror/language-data';

export type CodeSource = Readonly<{ kind: 'filename' | 'language'; value: string }>;

// Upstream Shell metadata omits .zsh, which the existing viewer supports.
// This is a narrow metadata supplement, not another language registry.
const extraExtensions: Readonly<Record<string, string>> = Object.freeze({ zsh: 'Shell' });

/** Filename patterns/extensions and fence names/aliases are distinct metadata namespaces. */
export function resolveCodeLanguage(source: CodeSource): LanguageDescription | null {
  if (source.kind === 'language') {
    return LanguageDescription.matchLanguageName(languages, source.value.trim(), false);
  }
  const filename = source.value.replaceAll('\\', '/').split('/').pop() ?? '';
  const dot = filename.lastIndexOf('.');
  const normalized = dot < 0 ? filename : filename.slice(0, dot) + filename.slice(dot).toLowerCase();
  const matched = LanguageDescription.matchFilename(languages, normalized);
  if (matched !== null) return matched;
  const extra = dot < 0 ? undefined : extraExtensions[normalized.slice(dot + 1)];
  return extra === undefined ? null : LanguageDescription.matchLanguageName(languages, extra, false);
}

export const CODE_HIGHLIGHT_LIMITS = Object.freeze({ characters: 262144, lines: 5000, lineLength: 16384 });

/** Bound parsing work without truncating the document or copied source. */
export function exceedsCodeHighlightLimits(text: string): boolean {
  if (text.length > CODE_HIGHLIGHT_LIMITS.characters) return true;
  let lines = 1;
  let lineLength = 0;
  for (const character of text) {
    if (character === '\n') {
      lines += 1;
      lineLength = 0;
    } else lineLength += character.length;
    if (lines > CODE_HIGHLIGHT_LIMITS.lines || lineLength > CODE_HIGHLIGHT_LIMITS.lineLength) return true;
  }
  return false;
}
