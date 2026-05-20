// Local ESLint plugin: `neige-calm`.
//
// Container for project-internal rules — currently just
// `no-persistent-in-usestate`. Wired into the flat config in
// `eslint.config.js` under the `neige-calm/` rule prefix.

const noPersistentInUsestate = require('./no-persistent-in-usestate.cjs');

module.exports = {
  rules: {
    'no-persistent-in-usestate': noPersistentInUsestate,
  },
};
