import { existsSync, readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { ESLint } from 'eslint';
import * as tsParser from '@typescript-eslint/parser';
import { describe, expect, it } from 'vitest';
import { createContextAllowlist, moduleRuntimeStateAllowlist } from './allowlists.mjs';
import { architecturePlugin } from './plugin.mjs';

const root = resolve(import.meta.dirname, '../..');
const fixtureRoot = resolve(import.meta.dirname, 'rule-fixtures');

async function lintFixture(ruleName: string, fixture: string, options?: Record<string, unknown>) {
  const file = resolve(fixtureRoot, fixture);
  const eslint = new ESLint({
    overrideConfigFile: true,
    overrideConfig: [{
      files: ['**/*.{ts,tsx}'],
      languageOptions: { parser: tsParser, parserOptions: { project: false } },
      plugins: { architecture: architecturePlugin },
      rules: { [`architecture/${ruleName}`]: options ? ['error', options] : 'error' },
    }],
  });
  const [result] = await eslint.lintText(readFileSync(file, 'utf8'), { filePath: file });
  return result.messages.filter((message) => message.ruleId === `architecture/${ruleName}`);
}

describe('architecture/no-module-runtime-state', () => {
  const rejected = [
    ['top-level-let.ts', 'let current'],
    ['mutable-constructor.ts', 'new Map'],
    ['mutable-object.ts', 'Object.freeze({})'],
    ['nested-mutable-object.ts', "Object.freeze({ cards: ['cards'] })"],
    ['mutable-array.ts', 'Object.freeze([] as const)'],
    ['lazy-singleton.ts', '(() =>'],
    ['factory-call.ts', 'createStore()'],
    ['as-const-array.ts', "Object.freeze(['a', 'b'] as const)"],
    ['logical-new.ts', 'new Map'],
    ['conditional-new.ts', 'new Map'],
    ['wrapped-new.ts', 'new Map'],
    ['sequence-new.ts', 'new Map'],
    ['unary-new.ts', 'new Map'],
    ['await-new.ts', 'new Map'],
    ['static-class-expression.ts', 'static current'],
    ['export-default-object.ts', 'specificMutableCache'],
    ['export-default-new.ts', 'new Map'],
    ['runtime-namespace.ts', 'new Map'],
    ['freeze-map.ts', 'Object.freeze'],
    ['freeze-nested-map.ts', 'Object.freeze'],
    ['custom-constructor.ts', 'new Store'],
    ['assignment.ts', 'f.cache'],
    ['assignment-primitive.ts', 'holder.current'],
    ['static-block-primitive.ts', 'R.count'],
    ['object-keys-unfrozen.ts', 'Object.freeze(Object.keys(X))'],
    ['router-factory.ts', 'createRouter'],
    ['file-route-factory.ts', 'createFileRoute'],
    ['static-accessor.ts', 'static accessor c'],
    ['top-level-mutable-argument.ts', 'new Map'],
    ['top-level-object-assign.ts', 'cache: new Map'],
    ['destructured-array.ts', 'Module runtime state'],
    ['schema-value.ts', 'z.object({}).parse(x)'],
  ] as const;
  for (const [fixture, entity] of rejected) {
    it(`rejects ${fixture}`, async () => {
      const messages = await lintFixture('no-module-runtime-state', fixture);
      expect(messages).toHaveLength(1);
      expect(messages.at(0)?.message).toContain(entity);
    });
  }

  const accepted = ['primitive.ts', 'function-declaration.ts', 'frozen-static-data.ts', 'frozen-nested-static-data.ts', 'frozen-functions.ts', 'safe-class-and-call.ts', 'declare-module.ts', 'schema.ts', 'schema-chained.ts', 'pure-factories.tsx'] as const;
  for (const fixture of accepted) {
    it(`accepts ${fixture}`, async () => {
      expect(await lintFixture('no-module-runtime-state', fixture)).toHaveLength(0);
    });
  }
  it('rejects every legacy mutable constructor family', async () => {
    const messages = await lintFixture('no-module-runtime-state', 'other-constructors.ts');
    expect(messages).toHaveLength(3);
    expect(messages.map(({ message }) => message)).toEqual(expect.arrayContaining([
      expect.stringContaining('new WeakMap'),
      expect.stringContaining('new EventTarget'),
      expect.stringContaining('new WebSocket'),
    ]));
  });
  it('rejects every writable primitive static', async () => {
    const messages = await lintFixture('no-module-runtime-state', 'static-primitives.ts');
    expect(messages).toHaveLength(3);
    for (const entity of ['static current', 'static flag', "static name = 'x'"]) {
      expect(messages.some(({ message }) => message.includes(entity)), entity).toBe(true);
    }
  });
  it('rejects mutation in a static block', async () => {
    const messages = await lintFixture('no-module-runtime-state', 'static-block.ts');
    expect(messages).toHaveLength(1);
    expect(messages.at(0)?.message).toContain('holder.current');
  });
  it('explains that shallow freeze leaves nested state mutable', async () => {
    const messages = await lintFixture('no-module-runtime-state', 'freeze-array-object.ts');
    expect(messages).toHaveLength(1);
    expect(messages.at(0)?.message).toContain('freeze nested objects/arrays too');
  });
  it('rejects frozen Object factories whose results can contain mutable values', async () => {
    const messages = await lintFixture('no-module-runtime-state', 'frozen-object-values.ts');
    expect(messages).toHaveLength(2);
    expect(messages.map(({ message }) => message)).toEqual(expect.arrayContaining([
      expect.stringContaining('Object.freeze(Object.entries(x))'),
      expect.stringContaining("Object.freeze(Object.fromEntries([['a', []]]))"),
    ]));
  });
  it('allows router factories only under a file-scoped rule option', async () => {
    expect(await lintFixture('no-module-runtime-state', 'router-factory.ts', { allowRouterFactories: ['createRouter'] })).toHaveLength(0);
    expect(await lintFixture('no-module-runtime-state', 'file-route-factory.ts', { allowRouterFactories: ['createFileRoute'] })).toHaveLength(0);
  });
});

describe('architecture/no-direct-persistence', () => {
  for (const fixture of [
    'persistence/direct-identifier.ts',
    'persistence/window-local-storage.ts',
    'persistence/computed-global-this.ts',
    'persistence/destructure-window.ts',
    'persistence/alias-session-storage.ts',
    'persistence/indexed-db-open.ts',
  ] as const) {
    it(`rejects ${fixture}`, async () => {
      expect(await lintFixture('no-direct-persistence', fixture)).toHaveLength(1);
    });
  }
  for (const fixture of ['core/keys/storage.ts', 'persistence/injected-port.ts'] as const) {
    it(`accepts ${fixture}`, async () => {
      expect(await lintFixture('no-direct-persistence', fixture)).toHaveLength(0);
    });
  }
});

describe('architecture/no-calm-key-outside-core-keys', () => {
  for (const fixture of ['calm-key/string-literal.ts', 'calm-key/template-head.ts'] as const) {
    it(`rejects ${fixture}`, async () => {
      expect(await lintFixture('no-calm-key-outside-core-keys', fixture)).toHaveLength(1);
    });
  }
  for (const fixture of ['core/keys/calm-key.ts', 'calm-key/injected-key.ts', 'calm-key/known-concatenation-escape.ts'] as const) {
    it(`accepts ${fixture}`, async () => {
      expect(await lintFixture('no-calm-key-outside-core-keys', fixture)).toHaveLength(0);
    });
  }
});

describe('architecture/no-class-dom-query', () => {
  for (const fixture of [
    'dom-selector/query-selector.ts',
    'dom-selector/query-selector-all.ts',
    'dom-selector/closest.ts',
    'dom-selector/matches.ts',
    'dom-selector/get-elements-by-class-name.ts',
    'dom-selector/dynamic.ts',
    'dom-selector/template.ts',
  ] as const) {
    it(`rejects ${fixture}`, async () => {
      expect(await lintFixture('no-class-dom-query', fixture)).toHaveLength(1);
    });
  }
  it('allows an exact third-party selector with an application container prefix', async () => {
    expect(await lintFixture('no-class-dom-query', 'dom-selector/allowlisted.ts', {
      allowlist: [{ file: 'rule-fixtures/dom-selector/allowlisted.ts', selector: '.file-viewer-code-wrap .cm-scroller' }],
    })).toHaveLength(0);
  });
  it('rejects an allowlist entry without a container prefix', async () => {
    expect(await lintFixture('no-class-dom-query', 'dom-selector/bare-allowlisted.ts', {
      allowlist: [{ file: 'rule-fixtures/dom-selector/bare-allowlisted.ts', selector: '.cm-scroller' }],
    })).toHaveLength(1);
  });
  it('accepts a static data selector', async () => {
    expect(await lintFixture('no-class-dom-query', 'dom-selector/data-selector.ts')).toHaveLength(0);
  });
  it('accepts a module-scope single-assignment const string without a class', async () => {
    expect(await lintFixture('no-class-dom-query', 'dom-selector/module-const-data.ts')).toHaveLength(0);
  });
  it('rejects a class in a module-scope single-assignment const as a class selector', async () => {
    const messages = await lintFixture('no-class-dom-query', 'dom-selector/module-const-class.ts');
    expect(messages).toHaveLength(1);
    expect(messages.at(0)?.messageId).toBe('classSelector');
  });
});

describe('architecture/no-core-platform-escape', () => {
  it('rejects globalThis.fetch', async () => {
    expect(await lintFixture('no-core-platform-escape', 'core-escape/global-this-fetch.ts')).toHaveLength(1);
  });
  it('rejects dynamic import()', async () => {
    expect(await lintFixture('no-core-platform-escape', 'core-escape/dynamic-import.ts')).toHaveLength(1);
  });
  it('accepts injected transports and static imports', async () => {
    expect(await lintFixture('no-core-platform-escape', 'core-escape/injected.ts')).toHaveLength(0);
  });
});

describe('architecture/no-create-context-outside-allowlist', () => {
  const rejected = [
    ['create-context-named.ts', 'createContext'],
    ['create-context-member.ts', 'React.createContext'],
    ['create-context-alias.ts', 'mk'],
    ['create-context-destructure.ts', 'mk'],
    ['create-context-indirect.ts', 'cc'],
    ['create-context-computed.ts', "React['createContext']"],
    ['react-state-member.ts', 'React.useState'],
    ['react-reducer-computed.ts', "React['useReducer']"],
  ] as const;
  for (const [fixture, entity] of rejected) {
    it(`rejects ${fixture}`, async () => {
      const messages = await lintFixture('no-create-context-outside-allowlist', fixture);
      expect(messages).toHaveLength(1);
      expect(messages.at(0)?.message).toContain(entity);
    });
  }
  for (const fixture of ['create-context-shadowed.ts', 'react-unrelated.ts'] as const) {
    it(`accepts unrelated or shadowed ${fixture}`, async () => {
      expect(await lintFixture('no-create-context-outside-allowlist', fixture)).toHaveLength(0);
    });
  }
  it('allows state hooks but still rejects createContext in the controlled outlet', async () => {
    const messages = await lintFixture('no-create-context-outside-allowlist', 'allow-react-state-hooks.ts', { allowReactStateHooks: true });
    expect(messages).toHaveLength(1);
    expect(messages.at(0)?.message).toContain('createContext');
  });
});

describe('architecture allowlists', () => {
  for (const [name, entries] of [
    ['module runtime state', moduleRuntimeStateAllowlist],
    ['createContext', createContextAllowlist],
  ] as const) {
    it(`${name} entries are explicit existing files`, async () => {
      expect(entries.some((entry) => /[*?{}[\]]/.test(entry))).toBe(false);
      for (const entry of entries) {
        expect(existsSync(resolve(root, entry)), entry).toBe(true);
        const rule = name === 'module runtime state' ? 'no-module-runtime-state' : 'no-create-context-outside-allowlist';
        const eslint = new ESLint({
          overrideConfigFile: true,
          overrideConfig: [{
            files: ['**/*.{ts,tsx}'], languageOptions: { parser: tsParser, parserOptions: { project: false } },
            plugins: { architecture: architecturePlugin }, rules: { [`architecture/${rule}`]: 'error' },
          }],
        });
        const file = resolve(root, entry);
        const [result] = await eslint.lintText(readFileSync(file, 'utf8'), { filePath: file });
        expect(result.messages.some((message) => message.ruleId === `architecture/${rule}`), `${entry} is stale`).toBe(true);
      }
    });
  }
});
