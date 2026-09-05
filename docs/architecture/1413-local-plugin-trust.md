# Local plugin execution trust boundary (#1413)

Status: accepted for the current single-owner, local plugin host.

## Decision

Local executable plugins are trusted code. Authorizing their installation and
execution, or granting write access that can replace their executed code, grants
code execution with the service's OS identity. The host does not sandbox local
app plugins. Plugin ids, process tokens and callback permissions enforce protocol
identity and API access; they do not isolate malicious native code from the
service's files, credentials or other same-identity processes.

This decision documents the existing execution model. It adds no new permission,
changes no persisted contract and does not make every plugin eligible for
privileged kernel APIs. Those API gates remain necessary.

## Evidence and scope

- `PluginProcess::spawn` executes the app manifest's entrypoint directly, without
  changing OS identity or creating an OS sandbox. It currently inherits the
  service environment before layering manifest variables and plugin credentials.
- Install, enable and reload belong to the REST tree protected by the owner
  session gate. Authentication currently has one implicit owner role.
  Development autologin bypasses that gate and must remain off in production.
- Installation records a disabled DB row. Merely discovering a new manifest does
  not auto-enable it. Boot selects enabled DB rows and resolves each id through
  the disk-loaded registry. Replacing an already enabled plugin's code therefore
  needs no new enable operation.
- Each app spawn mints a fresh token for that id; a replacement receives a new
  credential under the same identity, not the previous process's raw token.
- Unix local-path installation creates a symlink to the source tree. Trust extends
  to its resolved target, executable dependencies and parent directories that can
  replace them. It persists across rebuilds, reloads and service restarts. A
  protected `plugins_dir` alone does not protect a writable target tree.

This native-code boundary also matters for local CLI connectors. Their explicit
environment allowlist reduces accidental credential exposure but is not an OS
sandbox. Remote HTTP connectors do not spawn a local plugin child or receive an
app process token; their endpoint and secret-delivery rules remain separate.

## Loading policy

Keep symlink loading and support noncanonical directory names. Duplicate-id
arbitration prefers a directory name matching the manifest id, then sorted name
order. This is a deterministic conflict policy, not proof of origin or install
authorization. Requiring name equality cannot stop a writer from replacing the
canonical directory or its contents, and would drop valid noncanonical links.

## Implementation and acceptance

1. Publish the operator contract in `docs/plugin-security.md` and link it from the
   deployment guide and README. Cover the complete writable execution tree and
   the implications of linking an agent-editable worktree.
2. Share production HTTP/WS route assembly with integration tests, preserving the
   current session, actor, internal-loopback and public-route boundaries.
3. Exercise install, enable and reload with missing/invalid owner sessions and
   valid requests against real temp plugin trees and an in-memory SQL repo.
   Rejected requests must leave installation, DB, registry, tokens and process
   state unchanged. An authenticated control must actually install and execute
   the existing echo stub. No real Codex process is needed.
4. Mutation-check the session fence, run the existing auth and plugin registry
   tests, and run the Rust compile/lint/OpenAPI preflight. Review the final diff
   through two independent channels.

## Follow-up boundary

Deployment must not grant lower-trust users, build jobs or agents write access to
the execution tree unless they are intentionally trusted to execute as the
service. Documenting this model does not remediate an unsafe deployment.

Supporting untrusted local plugins requires a separate design: independently
protected installation authorization and artifacts, plus OS/process/filesystem
and credential isolation. A name check, digest stored beside writable code,
token rotation or installation authentication alone cannot provide that model.
Environment allowlisting for app children is a separate hardening change with a
compatibility review; inherited environment is current behavior, not a required
API contract introduced here.

Related: [#1413](https://github.com/keanji-x/neige-calm/issues/1413),
[#1168](https://github.com/keanji-x/neige-calm/issues/1168),
[#1402](https://github.com/keanji-x/neige-calm/pull/1402).
