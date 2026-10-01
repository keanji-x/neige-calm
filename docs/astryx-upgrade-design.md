# Astryx stable upgrade

Upgrade `fe` from `@astryxdesign/core` 0.1.3 to the current stable 0.6.3 release.
Keep the conversation, queue, attachment, and recovery contracts intact. Migrate
changed vendor APIs and remove old workarounds only when the new implementation
and behavior tests establish that they are redundant. Do not add simulated text
streaming or change persisted conversation outcomes as part of this upgrade.

## Acceptance

- Pin the stable version and regenerate the npm lockfile with the package manager.
- Preserve message submission, IME handling, trigger navigation, stop actions,
  attachments, model menus, tool detail state, and keyboard focus.
- Check theme targets and tokens against the installed vendor source.
- Run frontend lint, build, unit tests, browser tests, and repository text gates.
- Review the final diff through source-contract and behavioral verification
  channels, including layer boundaries, duplicate logic, and fixed assumptions.
