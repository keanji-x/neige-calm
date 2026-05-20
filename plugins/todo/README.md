# todo — Neige plugin

Per-card todo list. The iframe (`views/todo.html`) shows a checklist the user
can add to, tick off, and delete from; items persist in the plugin KV under
`card/<card_id>/items` so each todo card has an independent list.

The Rust binary is intentionally passive: it answers `initialize`,
`tools/list`, and `tools/call { name: "make_todo_card" }`, then idles. Every
mutation flows from the iframe via `neige.kv.*` routed through the host's
AppBridge fan-out.

## Build

This crate sits **outside** the kernel workspace (its own `[workspace]` table)
so the kernel's `cargo check` stays oblivious to it.

```bash
cd plugins/todo
cargo build --release
mkdir -p bin
cp target/release/todo bin/todo      # matches manifest.entrypoint.command
```

## Install + enable

```bash
curl -X POST localhost:3030/api/plugins/install \
  -H 'Content-Type: application/json' \
  -d "{\"source\":{\"kind\":\"local_path\",\"path\":\"$PWD\"}}"

curl -X POST localhost:3030/api/plugins/dev.neige.todo/enable
```

Or run `./demo.sh` for the whole dance. Unlike `hello-world`, this plugin
takes no wave id — there's nothing to inject into the manifest.

## What it does

- Tool: `make_todo_card` → returns `_meta.ui.resourceUri = ui://dev.neige.todo/list`.
- View: `list` → mounts `views/todo.html` as the card body.
- Storage: `neige.kv.get/set` against `card/<card_id>/items`. Item shape:
  `{ id, text, done, created_at }`. Quota: 64 KiB per plugin
  (`permissions.kv_quota_bytes`).
- Permissions: `cards_create: false`, `overlays_write: []`. The view's
  `permissions.tools` whitelist allows only `neige.kv.{get,set,delete}`.
