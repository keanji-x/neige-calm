import { readFileSync, readdirSync } from 'node:fs';
import { createRequire } from 'node:module';
import { dirname, extname, relative, resolve } from 'node:path';
import postcss from 'postcss';

// CSS-only existence audit, not a cascade/scope proof. TSX/JS inline styles and
// CodeMirror theme objects silently pass: they require browser coverage. Dynamic
// names are also out of scope. Only shipped CSS imports supply vendor names.
export function auditCustomProperties(files) {
  const definitions = new Set();
  const uses = [];
  const value = (text, file) => {
    // Skip CSS string tokens (including escaped quotes) and comments together so
    // comment-like text inside a string cannot expose a literal var().
    text = text.replace(/"(?:\\[\s\S]|[^"\\])*"|'(?:\\[\s\S]|[^'\\])*'|\/\*[\s\S]*?\*\//g, ' ');
    // Walking all var() starts also checks nested fallbacks independently.
    for (const match of text.matchAll(/\bvar\(\s*(--[\w\u00a0-\uffff-]+)\s*([,)])/g)) {
      if (match[2] === ')') uses.push({ name: match[1], file });
    }
  };
  for (const { file, code } of files) {
    if (extname(file) === '.css') {
      postcss.parse(code, { from: file }).walkDecls(decl => {
        if (decl.prop.startsWith('--')) definitions.add(decl.prop);
        if (!file.startsWith('node_modules/')) value(decl.value, file);
      });
    }
  }
  return uses.filter(({ name }) => !definitions.has(name))
    .map(({ file, name }) => `${file}: undefined custom property ${name} without fallback`);
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
      }
    }
  };
  walk(resolve(root, 'web/src'));
  return auditCustomProperties([...sources].map(([path, code]) => ({ file: relative(root, path), code })));
}

if (process.argv[1] && resolve(process.argv[1]) === resolve(import.meta.filename)) {
  const violations = checkCustomProperties(resolve(process.argv[2] ?? new URL('../..', import.meta.url).pathname));
  if (violations.length) {
    console.error(violations.join('\n'));
    process.exitCode = 1;
  } else console.log('CSS custom property references: valid');
}
