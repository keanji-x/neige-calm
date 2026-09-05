# Plugin host security

## Local plugins execute trusted code

Neige Calm's current local plugin host uses a single-owner trust model. Installing
and enabling a local executable plugin authorizes it to execute as the service's
OS identity. Local app children run without a host-created OS sandbox. They can
access files and resources available to that identity, subject to any external OS
restrictions on the service. Treat the ability to replace an enabled plugin's
code as the ability to execute code as the service on a subsequent spawn.

Installation initially records a disabled plugin. Boot automatically starts
enabled DB entries using the manifests loaded from disk; simply adding a new id
to the directory does not enable it. Once enabled, replacing its files does not
require another install or enable request. The host does not authenticate code
origin or pin the installed executable's content.

Plugin ids select registry, DB and API namespaces. App process tokens authenticate
the spawned process to the plugin protocol; each spawn receives a fresh token for
its id. Callback permissions constrain operations made through the kernel API.
These mechanisms, trusted-forge API gates and sandboxed browser views do not
provide OS isolation for native plugin code. In particular, a replacement app can
receive a fresh token under an already enabled plugin's id.

App children currently inherit the service environment, with manifest variables
layered on top and host-supplied plugin credentials. Do not assume unrelated
service secrets in that environment are hidden from app plugins. CLI connectors
use an explicit child environment allowlist, but their local executables are
also not OS-sandboxed by the host. Remote HTTP connectors do not receive app
process tokens or run as local children; review their configured destinations
and secrets separately.

## Protect the complete execution tree

On Unix, local-path installation links `plugins_dir/<id>` to the source directory.
It does not create an immutable copy. Rebuilding or editing the source changes
what a later spawn executes; reload also rereads its manifest. This is useful for
development and creates a continuing trust relationship with the source tree.

Restrict writes to the following to the service identity and administrators or
deployment jobs intentionally trusted to execute code as that identity:

- The configured plugin root, installed directories and symlinks.
- Resolved source trees, manifests, entrypoints, scripts and executable
  dependencies, including targets of symlinks within the plugin.
- Parent directories whose writers could rename or replace any of those paths.

Checking only the mode of `plugins_dir` is insufficient. Review ownership, group
writes and ACLs along both the installation path and its resolved target. A mode
such as `0700` on the root does not protect a source tree writable by another user.
Avoid installing from shared temporary directories or linking an agent-editable
worktree unless its writers are intentionally granted this execution authority.
An agent running as the service identity is not isolated by ordinary ownership
and mode checks against that same identity.

For deployment, use a dedicated service account with only the host access the
application needs, and keep production plugin sources in administrator-controlled
release directories rather than shared development trees. Restrict service
configuration, plugin data and credential files as well. External sandboxing can
limit the whole service's OS access; it does not by itself separate an app plugin
from the service inside that sandbox.

The directory-name rule only resolves duplicate manifest ids deterministically:
the entry named after its id wins, otherwise sorted name order wins. It is not
proof of identity. Noncanonical names and symlinks remain supported. Renaming an
entry to its id, checking a path string or rotating its token cannot make writable
code trustworthy.

## Owner authorization

Plugin management REST endpoints, including install, enable and reload, require
an owner session in production. The current authentication model has one owner
role, not a lower-privilege plugin installer role. Protect that session and its
login credentials as administrative access to service execution. A local-path
install names a directory on the server, not a file on the browser's machine.

Keep `CALM_DEV_AUTOLOGIN` / `auth.dev_autologin` disabled in production: enabling it
promotes every request to owner without a session. `X-Calm-Actor` is attribution,
not a substitute for owner authentication. Filesystem modification bypasses the
REST interface entirely, so API authentication does not protect writable plugin
code.

This contract does not make an unsafe deployment safe. If an unintended writer
can replace installed code or a link target, remove that access and restore code
from a trusted source before executing it again. Running untrusted local plugins
requires a separate installation and OS-isolation design.

See the [architecture decision](architecture/1413-local-plugin-trust.md) for the
scope and verification of this boundary.
