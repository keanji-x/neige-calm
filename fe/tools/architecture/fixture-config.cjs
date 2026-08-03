const config = require('../../.dependency-cruiser.cjs');

module.exports = { ...config, options: { ...config.options, exclude: undefined, tsConfig: undefined } };
