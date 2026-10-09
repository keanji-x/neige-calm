import { expect, it } from 'vitest';
import { resolveCodeLanguage, exceedsCodeHighlightLimits, CODE_HIGHLIGHT_LIMITS } from './public.tsx';

it.each([
  ['main.rs', 'Rust'], ['main.js', 'JavaScript'], ['main.cjs', 'JavaScript'], ['main.mjs', 'JavaScript'],
  ['main.ts', 'TypeScript'], ['main.cts', 'TypeScript'], ['main.mts', 'TypeScript'],
  ['main.jsx', 'JSX'], ['main.tsx', 'TSX'], ['main.py', 'Python'], ['main.go', 'Go'],
  ['main.java', 'Java'], ['main.json', 'JSON'], ['main.md', 'Markdown'], ['main.markdown', 'Markdown'],
  ['main.css', 'CSS'], ['main.html', 'HTML'], ['main.toml', 'TOML'], ['main.yaml', 'YAML'],
  ['main.yml', 'YAML'], ['main.sh', 'Shell'], ['main.bash', 'Shell'], ['main.zsh', 'Shell'],
  ['/repo/Dockerfile', 'Dockerfile'], ['/repo/PKGBUILD', 'Shell'], ['/repo/main.TSX', 'TSX'],
  ['C:\\repo\\main.rs', 'Rust'],
])('resolves supported filename %s through loadable metadata', async (value, name) => {
  const description = resolveCodeLanguage({ kind: 'filename', value });
  expect(description?.name).toBe(name);
  const support = await description?.load();
  expect(support?.language).toBeDefined();
});

it.each([
  ['rust', 'Rust'], ['JS', 'JavaScript'], ['ecmascript', 'JavaScript'], ['node', 'JavaScript'],
  ['ts', 'TypeScript'], [' JSX ', 'JSX'], ['tsx', 'TSX'], ['python', 'Python'],
  ['bash', 'Shell'], ['sh', 'Shell'], ['zsh', 'Shell'], ['yml', 'YAML'],
])('resolves declared language alias %s', (value, name) => {
  expect(resolveCodeLanguage({ kind: 'language', value })?.name).toBe(name);
});

it('keeps filename extensions and strict language labels in separate namespaces', () => {
  expect(resolveCodeLanguage({ kind: 'filename', value: 'main.cjs' })?.name).toBe('JavaScript');
  expect(resolveCodeLanguage({ kind: 'language', value: 'cjs' })).toBeNull();
  expect(resolveCodeLanguage({ kind: 'language', value: 'unknown-javascript-label' })).toBeNull();
  expect(resolveCodeLanguage({ kind: 'filename', value: '/folder.rs/README' })).toBeNull();
  expect(resolveCodeLanguage({ kind: 'filename', value: 'unknown.xyz123' })).toBeNull();
});

it('bounds characters, lines and individual line length without truncation', () => {
  expect(exceedsCodeHighlightLimits('fn main() {}')).toBe(false);
  expect(exceedsCodeHighlightLimits('x'.repeat(CODE_HIGHLIGHT_LIMITS.lineLength))).toBe(false);
  expect(exceedsCodeHighlightLimits('x'.repeat(CODE_HIGHLIGHT_LIMITS.lineLength + 1))).toBe(true);
  expect(exceedsCodeHighlightLimits('\n'.repeat(CODE_HIGHLIGHT_LIMITS.lines - 1))).toBe(false);
  expect(exceedsCodeHighlightLimits('\n'.repeat(CODE_HIGHLIGHT_LIMITS.lines))).toBe(true);
  const line = 'x'.repeat(1023) + '\n';
  expect(exceedsCodeHighlightLimits(line.repeat(CODE_HIGHLIGHT_LIMITS.characters / 1024))).toBe(false);
  expect(exceedsCodeHighlightLimits(line.repeat(CODE_HIGHLIGHT_LIMITS.characters / 1024) + 'x')).toBe(true);
});
