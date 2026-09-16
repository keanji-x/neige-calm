# Private remote access bundled with Neige

Linux releases include `neige-tailnet`, a separate userspace Tailscale node pinned
to tsnet 1.102.3. It serves only private Tailnet HTTPS on port 443. It does not use
the host's tailscaled socket, change Serve/Funnel, advertise routes, provide an
exit node, or enable host SSH. Other platforms are currently unsupported.

A fresh generated Neige configuration includes:

```toml
[tailnet]
provider = "private-tailnet"
hostname = "neige"
```

Remote access is initially disabled. In Settings → Network → Mobile connection,
choose Enable, then Start sign-in to authorize this computer's node. The login
link is only displayed for the current operation for two minutes; this is a UI
limit, not a claim about the Tailscale authorization link's expiry. Sign-in URLs
are not part of status, settings persistence, or service logs.

Enable MagicDNS and HTTPS certificates in your Tailnet DNS settings. Settings
reports node state, HTTPS readiness, the private address, and Neige readiness
separately. A ready node cannot bypass Neige pairing and owner approval. This
slice does not yet provision phone Tailnet membership from the pairing QR; phone
enrollment is tracked separately in #1712.

Existing explicit Funnel installations keep `--mobile-access-config` and omit
`[tailnet]` or set its provider to `disabled`. Selecting private-tailnet while
supplying a Funnel configuration is an error. Providers have separate control
state; the selected provider exclusively owns this deployment's pairing state.
Existing configuration files without a Tailnet section do not opt in on upgrade.

The default state is `<calm-data-dir>/tailnet`, separate from release files. It
contains node identity, desired.json, and private Unix control sockets. Paths can
be overridden with `state_dir` and `binary` in the Tailnet section; the state path
must be short enough for Unix sockets. The directory is 0700 and state/control
files are 0600. Only one app and one helper can own it. Never point it at a system
Tailscale directory. The fixed ingress is `ingress.sock` in the same private directory. Requests
can never choose another socket, network host, port, or main application route.

Disable persists the closed intent, closes the remote listener and live upgraded
connections, and retains node identity. Sign out of Tailnet is a separate action
that disables access, logs out, and deletes only this node's identity and its
local identity backups. If logout cannot be confirmed, access remains disabled
and the failure is reported. Enable it to retry sign-out. A network outage or
pending login/approval never causes a crash-restart loop.

The helper is a peer of the Neige kernel, not its descendant. Kernel restart
keeps the node running and returns a recognizable HTTP 503 while the fixed
restricted Unix ingress is unavailable. Kernel sessions remain in memory, so a kernel
restart can still require Neige pairing again. Full neige-app restart stops the
helper. Parent-death signaling and private state locks prevent orphan adoption
or two simultaneous writers. Five crashes in five minutes open a circuit; an
explicit Disable/Enable retries it.

The release manifest hashes the helper and its bundled third-party notices. Its
unit is `neigeTailnet`, with `deferUntilFullReboot`: the app pins the canonical
binary at startup, so switching a release symlink or a kernel-only restart cannot
accidentally load a new helper after a crash. Before opening retained state with
a changed binary, the app copies a private, stopped-writer snapshot and records
the binary hashes and pinned tsnet version. No backup is restored automatically.
The helper refuses an unknown state-version instead of guessing backward
compatibility. A future tsnet version change requires an explicitly validated
migration/rollback path; do not swap older binaries against newer state or undo
a user's logout by restoring a backup.

Build with `tailnet/build.sh` (Go from `tailnet/go.mod`), or `make build`. The alpha
builder includes the helper and notices. Isolated tests use private Unix/loopback fixtures and
fake nodes; actual account sign-in, Tailnet ACL/HTTPS setup, and two-device
connectivity still require a separately authorized acceptance run.

References: [tsnet Server API](https://tailscale.com/docs/reference/tsnet-server-api),
[Tailscale HTTPS setup](https://tailscale.com/docs/how-to/set-up-https-certificates).

The first release containing `neigeTailnet` needs a host bootstrap upgrade. The
older host has a closed set of manifest unit names, so its HTTP `/upgrade/apply`
cannot parse this new unit. Use the **new package's** `bin/neige-app` to run
`system upgrade --config <config> --package <release-directory>` and inspect its
preflight before rerunning with `--activate`. Complete the configured systemd
service restart (not merely the kernel `/restart`) so the new host and helper
unit become active. Preserve the data/config directories. Follow the normal
package hash verification and rollback procedure in the deployment runbook.
This path is separate from the old running host's apply endpoint; no parser
fallback or automatic deployment is introduced here.
