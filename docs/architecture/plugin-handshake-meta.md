# Kernel → plugin `_meta` contract

Audience: anyone writing a Neige `app` plugin, in this repo or outside it.

Everything the kernel tells a plugin *about itself* at startup rides in one
place: the `_meta` object of the MCP `initialize` **request** (`params._meta`).
This page is the contract for that object. It is not a description of one
feature's plumbing — it is the shape every future kernel-owned handshake datum
will use, so a plugin written against it keeps working as the set grows.

Producer: `crates/calm-server/src/plugin_host/mcp.rs`, `McpClient::initialize`.
Consumer example: `plugins/git-forge/main.rs`.

## The rules

**1. Every kernel-owned datum lives under a reverse-DNS namespace key.**

```jsonc
// initialize request
{
  "params": {
    "protocolVersion": "…",
    "capabilities": { … },
    "_meta": {
      "dev.neige/auth":   { "expected_echo": "…" },
      "dev.neige/config": { "values": { … } }
    }
  }
}
```

`dev.neige/*` is the kernel's. A plugin must ignore namespace keys it does not
recognise, and must not assume the set is closed — new ones are added without a
protocol-version bump.

**2. A namespace's value is always a kernel-owned object with named fields,
never a bare bag of the plugin's own keys.**

This is why the effective configuration arrives as `{"values": {…}}` and not
spliced directly under `dev.neige/config`. The keys inside `values` are named
by the *manifest author* and filled by the *operator*. If they sat at the
namespace's top level, the kernel could never add a sibling field there without
a plugin being unable to tell it apart from a configuration key of the same
name. One level of nesting buys that back permanently.

**3. Absence and emptiness are different sentences, and both are meaningful.**

| what arrives | what it means |
|---|---|
| the namespace key is absent | **this kernel does not deliver that datum.** An older kernel, or a client that carries none — e.g. the transport unit tests. A plugin that needs it should say so and degrade, not crash. |
| the key is present, payload empty (`{"values": {}}`) | **this kernel delivers it, and there is nothing to deliver.** For config: the plugin declares no `config_schema`, or every key is unset and undefaulted. |

Read this the right way round. `values == {}` is *not* "configuration is
unavailable, fall back to my own defaults" — the kernel has already merged the
manifest's declared `default`s in (see rule 4), so an empty object means the
merged result really was empty.

**4. `dev.neige/config.values` is already merged, and already enforced.**

What arrives is `manifest defaults ⊕ stored user_config` — the same value
`GET /api/plugins/{id}`'s `effective_config` reports, computed once in
`plugin_host::config::effective_config`. A plugin does not re-apply its own
manifest defaults and does not need to.

It also does not need to check `config_schema.required`: a plugin whose
required keys are unsatisfied is never started at all. It is refused before the
process is spawned, and the kernel reports it as `unavailable` with the missing
key names in `last_error`. So inside a running plugin, "required key present"
is an invariant, not a thing to validate.

**5. Nothing under `_meta` is echoed back, except where a rule says so.**

The response's `_meta` is for *claims the kernel verifies*. Today there is
exactly one: `dev.neige/auth.echoed_token` must mirror the
`expected_echo` the kernel sent, or the process is killed. Configuration has no
such claim — demanding a mirror would turn "the plugin ignores configuration it
does not understand" into a failed handshake — so a plugin sends nothing back
for it.

## Minimal consumer

```rust
// inside the plugin's `initialize` handler, given the request frame
let config = frame
    .pointer("/params/_meta/dev.neige~1config/values")   // `~1` escapes the `/`
    .and_then(Value::as_object);

match config {
    None => { /* kernel delivers no config; run on the plugin's own fallbacks */ }
    Some(values) => { /* authoritative — already merged, required keys present */ }
}
```

## Where the wire shape is pinned

`crates/calm-server/tests/cases/plugin_config_delivery.rs` boots a real child
process and asks *the plugin* what it received, so the assertions above are
witnessed on the wire rather than against the kernel's own merge.
