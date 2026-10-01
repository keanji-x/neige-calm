# Readonly task permission profile

Readonly tasks use a trusted named Codex permission profile, rather than the
legacy read-only sandbox which allows reading the filesystem root. The installed
Codex 0.159.2 protocol makes named permissions mutually exclusive with sandbox
overrides at thread start, resume, and turn start. Writer calls keep their wire.

The lease-scoped `neige-task-read-<UUID>` profile denies the root, permits the platform
minimal tool set and workspace reads, and disables network access. The application
supplies explicit absolute protected runtime paths and additional readable roots
for Git common directories and separated execution helpers. It must never grant a
helper parent that contains private runtime configuration. Explicit denies protect
runtime files within otherwise readable workspaces. This profile does not claim
to restrict the provider daemon itself, which needs its operator credentials.

The trusted shared-home writer installs each immutable lease-scoped profile and rejects conflicting reuse
instead of merging operator or project overrides. Callers select one typed permission choice; the SDK
does not infer task access or silently downgrade to root-readable sandbox mode.
Application ownership decides task access once and applies the choice to every
thread lifecycle entry point. The shared daemon must receive this configuration
before loading it; configuration changes require its normal restart lifecycle.

Acceptance checks cover actual fake RPC parameters, profile replacement and path
validation, and native sandbox reads of temporary ordinary files only. Native
runtime may grant helper directories independently, so a temporary filesystem
check and runtime source review must verify the final effective policy. No model,
identity service, business API, or real private file is used for these checks.

Protocol reference: installed Codex-generated v2 ThreadStartParams,
ThreadResumeParams, TurnStartParams, and CommandExecParams schemas. Configuration
reference: Codex rust-v0.159.2 core/config.schema.json and
core/src/config/permissions.rs.

The native dummy check uses the direct `codex-linux-sandbox` entry point, which
[`arg0/src/lib.rs`](https://github.com/openai/codex/blob/rust-v0.159.2/codex-rs/arg0/src/lib.rs)
dispatches before loading dotenv or application configuration. It supplies the
canonical runtime profile described by
[`protocol/src/models.rs`](https://github.com/openai/codex/blob/rust-v0.159.2/codex-rs/protocol/src/models.rs)
and a separated temporary helper directory. With ordinary dummy files, workspace
and Git common directory reads passed, while a protected runtime directory nested
inside the workspace, an outside file, and workspace writes were denied. This
checks the compiled filesystem policy; named configuration resolution and live
provider task execution remain separate integration checks. The regular sandbox
CLI loads configuration even with explicit sandbox state, so it was not used for
this dummy check. No real home configuration or private file was read.
