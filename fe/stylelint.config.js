import layerBoundaries from './tools/styles/stylelint-plugin.mjs';

export default {
  extends: ['stylelint-config-recommended'],
  plugins: [layerBoundaries],
  rules: {
    'neige-calm/unlayered-cm-scope': [true, {
      unlayeredExceptions: [],
    }],
  },
};
