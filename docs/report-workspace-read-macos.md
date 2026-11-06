# Report workspace reads on macOS

Review tier: **L2**, because enabling workspace reads crosses a filesystem
isolation boundary. Two independent final reviews are arranged by the planner.

## Outcome and ownership

Report links to `README.md` and `docs/README.md` must reach the selected Track's
persisted workspace through both existing text and image/raw handlers. Both use
the shared `routes::fs` secure opener. Linux retains its `openat2` resolution
policy. Darwin's constrained backend also opens attachment files and directory
descriptors; the independent owner-only host APIs are unchanged.

The existing relative-path validator runs before root I/O. The trusted persisted
root may have host aliases (including `/var` on macOS); its opened directory fd
is the authority. User-controlled descendants are resolved once by the kernel.
macOS uses relative `openat` with `O_NOFOLLOW_ANY | O_RESOLVE_BENEATH |
O_CLOEXEC | O_NONBLOCK`, verifies the returned fd is a regular file, and hands
that same fd to the existing capped readers. No canonicalize/reopen or userspace
component walk is allowed. Text UTF-8/truncation, raw extension/cap/headers and
missing/denied/malformed classifications remain with their existing owners.

## Kernel contract and supported runtime

The macOS policy requires Darwin 25 (macOS 26) or newer. This is a conservative
support floor, not a claim about first availability. Apple documents whole-path
symlink rejection and rejection when resolution escapes the directory fd in
[open(2)](https://github.com/apple-oss-distributions/xnu/blob/xnu-12377.121.6/bsd/man/man2/open.2).
The local typed flag values come from Apple's
[fcntl.h](https://github.com/apple-oss-distributions/xnu/blob/xnu-12377.121.6/bsd/sys/fcntl.h);
no dependency upgrade is needed. Apple also supplies
[resolve-beneath tests](https://github.com/apple-oss-distributions/xnu/blob/xnu-12377.121.6/tests/vfs/resolve_beneath.c).

Before opening any requested descendant, require a valid kernel release and an
absolute `/` open against the root fd to fail with `ENOTCAPABLE` under the same
constraints. The probe reads no directory contents; an unexpected successful fd
is closed. Unknown releases, unsupported flags, and unexpected probe outcomes
disable this filesystem capability. Never retry with weaker flags. The version floor
and refusal probe prevent old kernels silently ignoring unknown flag bits.

This design trusts Apple's public pathname-resolution contract, including under
rename, rather than claiming a full XNU/APFS proof or Linux semantic equivalence.
The probe tests flag enforcement, not concurrency. Deterministic fd replacement
tests and stress experiments can expose defects; zero observed escapes is not
a proof. A concrete contract contradiction or reproducible escape stops this
backend proposal instead of relaxing isolation.

All macOS descendant symlinks, internal or external, leaf or parent, are refused.
Track reads return a 400 explaining that macOS workspace paths disallow symlinks. Linux retains
its existing in-root symlink behavior. Hard links and mount contents remain
governed by the selected directory boundary, as in the existing Linux contract.

## UI and verification

Report owns its grid placement: wrap `FileReadError` in its existing error
column. Keep shared error presentation, 5xx detail suppression and Retry intact.
Check sibling loading/source/image owners. Extend the real router fixture and
browser tests with HTTP 500, generic error, a real pointer Retry, and repository
README rendering at 1000/1440 desktop and mobile, short/long Report, with/without
Conversation. Geometry and hit tests must precede interaction auto-scrolling.

First register only the persisted A/B-root and missing-path handler tests on
macOS; both must fail against the placeholder. Then verify production handlers,
validation before root I/O, symlinks, special files, fd binding after parent/root
replacement, caps, UTF-8 and response headers. Mutation-check only critical
isolation assertions, with a declared exact red set and safe restoration.
Preserve Linux tests; macOS results do not validate the Linux kernel path.
Worker browser sandbox launch is unavailable: hand normal repository commands
to the planner, and do not claim browser success before they run.

## Risk and rollback

The conservative policy intentionally rejects in-root macOS symlinks and older
macOS releases. Runtime refusal is a path-free, actionable 400 asking for a supported server
OS and permitted filesystem operations. Image admission and the run response
`attachments_supported` consume the same capability as historical attachment
reads. Revert the shared Darwin dispatch to disable the capability if kernel
behavior violates the contract. No persisted schema changes are
needed; keep the independent UI placement correction and earlier menu/prompt
work. Required gates and two fresh L2 reviews precede acceptance.


## Attachment boundary (#3443)

Only the persisted workspace root is trusted to contain host aliases. The
`.neige/attachments/<card>/{staging,bound}` chain is untrusted. Upload creation
opens each child beneath its held parent descriptor; bind opens the card through
the workspace descriptor and then retains that card authority for both child
directories. Historical reads resolve the entire attachment-relative path under
the workspace root, including `.neige` and `attachments`. No attachment ancestor
is reopened as a trusted root. Single-component names, exclusive 0600 temporary
creation, 0700 directories, atomic rename, sync and dropped-upload cleanup remain
with the existing attachment owner.

Managed workspace and provider eligibility remain independent requirements;
attached workspaces still refuse attachments. The runtime capability probe does
not require the lazy managed workspace to exist. Unsupported kernels refuse
before attachment I/O with the same stable, path-free upgrade guidance.

Production-router regressions cover upload/readback, private permissions,
ancestor and leaf links, replacement during streaming, interrupted uploads,
cross-card refusal, binding and binding retries. The focused worker commands are
limited to `calm-server` lib filters `planner_attachments::`/`routes::fs::` and
`planner_harness_suite` filters `planner_attachments_rest::`/`planner_attachments_bind::`.
The kernel gate owns text, Rust quick and L2 contract checks. CI owns broad suites;
no real Codex E2E runs on the shared host. No permanent test-name inventory is
introduced: ordinary added tests are discovered by the existing module filters.
