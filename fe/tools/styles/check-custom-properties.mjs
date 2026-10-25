import { readFileSync, readdirSync } from 'node:fs';
import { createRequire } from 'node:module';
import { dirname, extname, relative, resolve } from 'node:path';
import postcss from 'postcss';
import ts from 'typescript';

// CSS-use existence audit, not a cascade/scope proof. Production TS/TSX
// literals contribute definitions only; their uses require browser coverage.
// Dynamic names are out of scope. Only shipped CSS imports supply vendor names.
export function auditCustomProperties(files) {
  const definitions = new Set();
  const uses = [];
  const value = (text, file) => {
    for (const name of references(text)) uses.push({ name, file });
  };
  for (const { file, code } of files) {
    if (extname(file) === '.css') {
      postcss.parse(code, { from: file }).walkDecls(decl => {
        if (decl.prop.startsWith('--')) definitions.add(decl.prop);
        if (!file.startsWith('node_modules/')) value(decl.value, file);
      });
    } else if (extname(file) === '.ts' || extname(file) === '.tsx') {
      const source = ts.createSourceFile(file, code, ts.ScriptTarget.Latest, false,
        file.endsWith('.tsx') ? ts.ScriptKind.TSX : ts.ScriptKind.TS);
      const visit = node => {
        // Quoted object keys are string literals too. Any exact-name literal
        // counts, without trying to infer runtime style ownership or execution.
        if ((ts.isStringLiteral(node) || ts.isNoSubstitutionTemplateLiteral(node))
          && /^--[\w\u00a0-\uffff-]+$/.test(node.text)) definitions.add(node.text);
        ts.forEachChild(node, visit);
      };
      visit(source);
    }
  }
  return uses.filter(({ name }) => !definitions.has(name))
    .map(({ file, name }) => `${file}: undefined custom property ${name} without fallback`);
}

// A small CSS Syntax 3 token walk. Keep opaque tokens in the stream so they
// cannot join the two sides of a var() argument into a reference.
function* references(text) {
  const chars = Array.from(text.replace(/\r\n?|\f/g, '\n'));
  const whitespace = char => char === ' ' || char === '\t' || char === '\n';
  const nameChar = char => char !== undefined && /[\w\u00a0-\uffff-]/u.test(char);
  const hex = char => char !== undefined && /[0-9a-f]/i.test(char);
  let i = 0;
  const escape = () => {
    i++; // backslash
    if (hex(chars[i])) {
      let count = 0;
      while (count < 6 && hex(chars[i])) { i++; count++; }
      if (whitespace(chars[i])) i++;
    } else if (i < chars.length) i++; // one Unicode code point, also in strings
  };
  const skipWhitespace = () => { while (whitespace(chars[i])) i++; };
  const token = () => {
    while (i < chars.length) {
      if (whitespace(chars[i])) { i++; continue; }
      if (chars[i] === '/' && chars[i + 1] === '*') {
        i += 2;
        while (i < chars.length && !(chars[i] === '*' && chars[i + 1] === '/')) i++;
        i = Math.min(i + 2, chars.length);
        continue;
      }
      break;
    }
    if (i === chars.length) return { kind: 'end' };
    const char = chars[i];
    if (char === '"' || char === "'") {
      i++;
      while (i < chars.length && chars[i] !== char && chars[i] !== '\n') {
        if (chars[i] === '\\') escape();
        else i++;
      }
      if (chars[i] === char) i++;
      return { kind: 'opaque' };
    }
    if (nameChar(char) || char === '\\') {
      let name = '';
      let escaped = false;
      while (i < chars.length) {
        if (nameChar(chars[i])) name += chars[i++];
        else if (chars[i] === '\\' && chars[i + 1] !== '\n') {
          escaped = true;
          escape();
        } else break;
      }
      // Invalid escapes outside strings are delimiters; always make progress.
      if (!name && !escaped) i++;
      if (chars[i] === '(') {
        i++;
        if (!escaped && name.toLowerCase() === 'url') {
          skipWhitespace();
          if (chars[i] !== '"' && chars[i] !== "'") {
            // URL and bad-URL remnants both end at the first unescaped ')'.
            // Quotes, comments and '(' inside them never open nested tokens.
            while (i < chars.length && chars[i] !== ')') {
              if (chars[i] === '\\') escape();
              else i++;
            }
            if (chars[i] === ')') i++;
            return { kind: 'opaque' };
          }
        }
        return { kind: 'function', name: escaped ? '' : name };
      }
      return { kind: 'ident', name: escaped ? '' : name };
    }
    i++;
    return { kind: char };
  };
  let argument = false;
  let reference;
  while (i < chars.length) {
    const next = token();
    if (reference && next.kind === ')') yield reference;
    reference = argument && next.kind === 'ident' && next.name.startsWith('--')
      && next.name.length > 2 ? next.name : undefined;
    // Do not consume a fallback as a unit: its var() calls need their own checks.
    argument = next.kind === 'function' && next.name === 'var';
  }
}

export function checkCustomProperties(root) {
  const require = createRequire(resolve(root, 'package.json'));
  const sources = new Map();
  const addCss = path => {
    if (sources.has(path)) return;
    const code = readFileSync(path, 'utf8');
    sources.set(path, code);
    postcss.parse(code).walkAtRules('import', rule => {
      const target = /^(?:url\(\s*)?["']?([^"')\s]+)/.exec(rule.params)?.[1];
      if (!target) throw new Error(`Cannot read CSS import in ${path}`);
      addCss(target.startsWith('.') ? resolve(dirname(path), target) : require.resolve(target));
    });
  };
  addCss(resolve(root, 'web/src/styles/entry.css'));
  const walk = directory => {
    for (const entry of readdirSync(directory, { withFileTypes: true })) {
      const path = resolve(directory, entry.name);
      if (entry.isDirectory()) walk(path);
      else if (!path.includes('.test.') && !path.includes('.spec.')) {
        if (path.endsWith('.module.css')) addCss(path);
        else if (path.endsWith('.ts') || path.endsWith('.tsx')) sources.set(path, readFileSync(path, 'utf8'));
      }
    }
  };
  walk(resolve(root, 'web/src'));
  walk(resolve(root, 'core'));
  return auditCustomProperties([...sources].map(([path, code]) => ({ file: relative(root, path), code })));
}

if (process.argv[1] && resolve(process.argv[1]) === resolve(import.meta.filename)) {
  const violations = checkCustomProperties(resolve(process.argv[2] ?? new URL('../..', import.meta.url).pathname));
  if (violations.length) {
    console.error(violations.join('\n'));
    process.exitCode = 1;
  } else console.log('CSS custom property references: valid');
}
