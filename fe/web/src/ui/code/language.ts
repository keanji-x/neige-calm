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
  const extension = dot < 0 ? '' : normalized.slice(dot + 1);
  const extra = Object.hasOwn(extraExtensions, extension) ? extraExtensions[extension] : undefined;
  return extra === undefined ? null : LanguageDescription.matchLanguageName(languages, extra, false);
}

export const CODE_HIGHLIGHT_LIMITS = Object.freeze({ characters: 262144, lines: 5000, lineLength: 16384 });

/** Bound parsing work without truncating the document or copied source. */
export function exceedsCodeHighlightLimits(text: string): boolean {
  if (text.length > CODE_HIGHLIGHT_LIMITS.characters) return true;
  let lines = 1;
  let lineLength = 0;
  for (let index = 0; index < text.length; index += 1) {
    const character = text[index];
    if (character === '\r' || character === '\n') {
      lines += 1;
      lineLength = 0;
      if (character === '\r' && text[index + 1] === '\n') index += 1;
    } else lineLength += 1;
    if (lines > CODE_HIGHLIGHT_LIMITS.lines || lineLength > CODE_HIGHLIGHT_LIMITS.lineLength) return true;
  }
  return false;
}
