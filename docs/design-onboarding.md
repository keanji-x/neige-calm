# Packaged developer onboarding

Issue: https://github.com/keanji-x/neige-calm/issues/2672

Outcome: a Linux developer with an authenticated coding CLI installs the verified
package without compilers or systemd, completes a repository investigation,
understands templates and approvals, and can continue to a small development
result. Validate this with a fresh-context agent inside Docker and fix the
observed confusion before repeating the exercise.

Review tier: L2. Fresh installation activates release paths and initializes
private configuration and persisted installation identity. Template availability
must remain the owning server component's decision.

## Ownership and scope

- `neige-app system setup` owns fresh-only installation using the existing v2
  package verifier and installed-state representation. Refuse existing state,
  tampered packages, unsafe destinations, and insecure credentials. Keep the
  generated configuration, admin token, plugins and data private and isolated.
  Foreground startup works without a systemd unit. Existing upgrade, rollback,
  authorization and merge approval remain authoritative.
- Template authors own purpose descriptions. The server declares readiness from
  the registered required owner and its actual lifecycle state. The UI displays
  that contract and links to existing settings without special-casing template
  identities or changing plugin enablement implicitly. Keep unfinished creation
  drafts when setup is repaired.
- Recipes save ordinary Markdown working instructions and the desired report
  structure. They do not pre-enqueue tasks or grant plugin authority.
- User guides cover installation, a first report, development with an optional
  Issue, supervision and recovery. Maintainer builds and advanced operations
  remain separate references.

## Verification ownership

Implementers run focused production tests (red/green for defects), formatter and
lint fixes. This integration worktree runs required text/contract/quick Rust
gates, frontend gates, targeted browser checks and tier-1 integration checks as
selected by changed surfaces. CI owns broad Rust suites. Two independent
read-only reviews inspect the frozen integration diff, including abstraction
boundaries, duplicate policy and hardcoded application assumptions. Both review
channels rerun fresh after behavioral fixes.

Load-bearing installation refusal assertions receive a predicted single-factor
production mutation in an exclusive implementation worktree and a green replay
after restoration. No other writer or reviewer sees transient mutations.

## Cold-user exercise

The fresh-context agent receives the package, runtime location and a user goal,
not the implementation plan. It follows only shipped user guidance. A clean
Docker home has no Rust, Node or Go; repository and application state are local
to the container. Authorized Codex executable/authentication are mounted read-only;
provider-only proxy egress and private PID/network namespaces prevent host-service
access. Only deliberately exposed user UI traffic may enter the sandbox.

Exercise real owner login, provider readiness, Area creation, an Investigation
Track and report persistence, then a bounded small local repository change.
No external PR or merge is part of the sandbox task. Ask the agent to explain
Area/Track/Report, template choice, optional Issue, plugin prerequisites, human
approval, restart and recovery. Record actual commands, evidence and unanswered
questions; fix product/documentation issues and run a fresh cold pass. Do not
count a mocked model or an already configured container as a clean first run.
