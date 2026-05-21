// Local ESLint plugin: `neige-calm`.
//
// Container for project-internal rules. Wired into the flat config in
// `eslint.config.js` under the `neige-calm/` rule prefix.

const noReactStateHookMembers = require('./no-react-state-hook-members.cjs');
const noPersistentInUsestate = require('./no-persistent-in-usestate.cjs');
const noRawPrimitiveRole = require('./no-raw-primitive-role.cjs');

module.exports = {
  rules: {
    'no-react-state-hook-members': noReactStateHookMembers,
    'no-persistent-in-usestate': noPersistentInUsestate,
    'no-raw-primitive-role': noRawPrimitiveRole,
  },
};
