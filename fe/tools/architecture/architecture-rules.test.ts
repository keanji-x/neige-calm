import { existsSync, readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { ESLint } from 'eslint';
import * as tsParser from '@typescript-eslint/parser';
import { describe, expect, it } from 'vitest';
import { createContextAllowlist, moduleRuntimeStateAllowlist } from './allowlists.mjs';
import { architecturePlugin } from './plugin.mjs';

const root = resolve(import.meta.dirname, '../..');
const fixtureRoot = resolve(import.meta.dirname, 'rule-fixtures');

async function lintFixture(ruleName: string, fixture: string) {
  const file = resolve(fixtureRoot, fixture);
  const eslint = new ESLint({
    overrideConfigFile: true,
    overrideConfig: [{
      files: ['**/*.{ts,tsx}'],
      languageOptions: { parser: tsParser, parserOptions: { project: false } },
      plugins: { architecture: architecturePlugin },
      rules: { [`architecture/${ruleName}`]: 'error' },
    }],
  });
  const [result] = await eslint.lintText(readFileSync(file, 'utf8'), { filePath: file });
  return result.messages.filter((message) => message.ruleId === `architecture/${ruleName}`);
}

describe('architecture/no-module-runtime-state', () => {
  const rejected = [
    ['top-level-let.ts', 'let current'],
    ['mutable-constructor.ts', 'new Map'],
    ['mutable-object.ts', 'cache = {}'],
    ['mutable-array.ts', 'Object.freeze([] as const)'],
    ['static-mutable.ts', 'static current'],
    ['lazy-singleton.ts', '(() =>'],
    ['factory-call.ts', 'createStore()'],
    ['as-const-array.ts', "Object.freeze(['a', 'b'] as const)"],
    ['logical-new.ts', 'new Map'],
    ['conditional-new.ts', 'new Map'],
    ['wrapped-new.ts', 'new Map'],
    ['sequence-new.ts', 'new Map'],
    ['unary-new.ts', 'new Map'],
    ['await-new.ts', 'new Map'],
    ['static-block.ts', 'Registry.current'],
    ['static-class-expression.ts', 'static current'],
    ['export-default-object.ts', 'cache'],
    ['export-default-new.ts', 'new Map'],
    ['runtime-namespace.ts', 'new Map'],
    ['freeze-map.ts', 'Object.freeze'],
    ['freeze-nested-map.ts', 'Object.freeze'],
    ['freeze-array-object.ts', 'Object.freeze'],
    ['custom-constructor.ts', 'new Store'],
    ['assignment.ts', 'f.cache'],
  ] as const;
  for (const [fixture, entity] of rejected) {
    it(`rejects ${fixture}`, async () => {
      const messages = await lintFixture('no-module-runtime-state', fixture);
      expect(messages).toHaveLength(1);
      expect(messages.at(0)?.message).toContain(entity);
    });
  }

  const accepted = ['primitive.ts', 'function-declaration.ts', 'frozen-static-data.ts', 'frozen-nested-static-data.ts', 'declare-module.ts', 'schema.ts', 'schema-chained.ts', 'pure-factories.tsx'] as const;
  for (const fixture of accepted) {
    it(`accepts ${fixture}`, async () => {
      expect(await lintFixture('no-module-runtime-state', fixture)).toHaveLength(0);
    });
  }
  it('rejects every legacy mutable constructor family', async () => {
    expect(await lintFixture('no-module-runtime-state', 'other-constructors.ts')).toHaveLength(3);
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
