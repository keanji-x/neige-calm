import layerBoundaries from './tools/styles/stylelint-plugin.mjs';
import { readFileSync } from 'node:fs';
import { parse } from 'yaml';

const unlayeredExceptions = parse(readFileSync(
  new URL('./web/src/styles/unlayered-exceptions.yaml', import.meta.url), 'utf8',
)).map((entry) => entry.path);

export default {
  extends: ['stylelint-config-recommended'],
  plugins: [layerBoundaries],
  rules: {
    'neige-calm/unlayered-cm-scope': [true, {
      unlayeredExceptions,
    }],
  },
};
