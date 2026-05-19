# hello-world — Neige M3 reference plugin

Minimal plugin that exercises the full M3 pipeline (manifest → install → spawn
→ MCP handshake → host callback → WS event) per
[`docs/m3-design.md`](../../docs/m3-design.md) §9.

On enable, the kernel spawns this binary, performs the slice-H auth handshake,
and the plugin sends one `neige.overlay.set` that flips the configured wave's
status overlay to `running`. The existing UI (`web-calm/src/api/adapt.ts`)
renders that as a pulsing green ring on the wave header.

## Build

This crate sits **outside** the kernel workspace (it has its own `[workspace]`
table) so the kernel's `cargo check` stays oblivious to it.

```bash
cd plugins/hello-world
cargo build --release
mkdir -p bin
cp target/release/hello-world bin/hello-world      # matches manifest.entrypoint.command
```

## Install + enable

The kernel only accepts the local-path install source in M3:

```bash
# 1. Edit manifest.json — set entrypoint.env.NEIGE_DEMO_WAVE to a real wave id.
#    Pick one from the UI or `curl localhost:3030/api/waves`.

# 2. Install + enable.
curl -X POST localhost:3030/api/plugins/install \
  -H 'Content-Type: application/json' \
  -d "{\"source\":{\"kind\":\"local_path\",\"path\":\"$PWD\"}}"

curl -X POST localhost:3030/api/plugins/dev.neige.hello-world/enable
```

Or run `./demo.sh <wave-id>` for the whole dance.

## What you should see

A WS event tail (`curl -N localhost:3030/api/events`) reveals:

```
{"ev":"plugin.state","data":{"id":"dev.neige.hello-world","state":"spawning"}}
{"ev":"plugin.state","data":{"id":"dev.neige.hello-world","state":"running"}}
{"ev":"overlay.set",  "data":{"plugin_id":"dev.neige.hello-world",
                              "entity_kind":"wave","entity_id":"<wave>",
                              "kind":"status","payload":{"state":"running"}}}
```

The UI flips that wave to running immediately.

## Demo-wave configuration

M3 slice B does **not** propagate `user_config` into the plugin's environment;
it only injects `NEIGE_PLUGIN_TOKEN`, `NEIGE_PLUGIN_ID`, and
`NEIGE_PLUGIN_DATA_DIR`. The manifest's `entrypoint.env` block is layered on
top, so for now the wave id ships **inside the manifest** (edit before
install). When slice C grows a `neige.kv.get` round-trip, a future iteration of
this plugin can read the wave id from KV instead. Until then: edit
`manifest.json` and reinstall, or `POST /api/plugins/.../disable` then patch
the manifest on disk and `POST .../enable`.

## Iframe view

`views/status.html` is wired into the manifest but the iframe runtime
(M3 slice F) doesn't render plugin cards yet. The kernel will still serve the
HTML at `GET /api/plugins/dev.neige.hello-world/views/status` and set the
slice-H iframe cookie — useful for poking with `curl` while the UI lands.
