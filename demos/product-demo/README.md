# Frontend-driven product demo

A continuous desktop scene built around the actual Neige React application. A browser-memory transport supplies one isolated sample Area and Track; native controls create them, accept the request and follow-up, prepare the Report in the production Worker, hover the Markdown instructions, and display an interactive HTML preview. Pixel characters, dialogue and camera motion share a deterministic clock.

The demonstration is scripted. It does not start real Codex/Claude sessions, send email, write a repository, or call a production backend. The Codex → Claude → Codex → Report relay is a visual explanation of delegation. No pre-recorded screenshots or videos are loaded by the scene.

- [Full film, 49 seconds](videos/neige-codex-claude-relay.mp4)
- [Relay excerpt, 9 seconds](videos/neige-relay-excerpt.mp4)
- [Video provenance and checksums](videos/manifest.json)

## Run

Use Node.js 24+, the repository's locked frontend dependencies, and Chromium. There is no second dependency tree or lockfile for this tool.

```sh
# From the repository root:
(cd fe && npm ci && npx playwright install chromium)
cd demos/product-demo
npm run check
npm run dev -- ../../fe
```

Open <http://localhost:4187/> and press Play. Pause, scrub, reset, or operate the native page directly. All sample state stays in the browser. Keep the sidebar and expanded Progress section visible; the scene compacts the native conversation drawer to leave room above it.

```sh
# Build a self-contained preview, without a Neige backend:
npm run build -- ../../fe
python3 -m http.server 4188 --bind 127.0.0.1 --directory output/live-dist
# In a second terminal from this directory:
npm run test:browser -- http://127.0.0.1:4188
```

The browser check exercises the actual creation forms, input, conversation, Worker-prepared Report, relay, hover preview and HTML theme buttons. It also verifies rewind, source-media absence, zero backend HTTP requests, and no browser errors. A separate player check covers rewinding into the creation intro, overlapping seek/reset requests, rapid keyboard scrubbing, and resetting during an in-flight playback operation. All producers share one serialized latest-request queue. Scene/model type checks exclude the integrated app adapter; the real build and browser check verify that integration.

## Export and verify

Install FFmpeg (`ffmpeg` and `ffprobe`) for video export and verification.

```sh
# With the preview server above still running:
npm run render -- http://127.0.0.1:4188 output/demo.mp4
npm run verify:videos
# Optional excerpt, preserving the approved edit:
ffmpeg -y -ss 21.4 -i output/demo.mp4 -t 9 -c:v libx264 -preset fast -crf 19 \
  -af 'afade=t=in:d=0.15,afade=t=out:st=8.5:d=0.5' -c:a aac \
  -movflags +faststart output/relay.mp4
```

Export renders the live DOM through the same seek interface used by the player and feeds frames directly to FFmpeg. It writes no source-frame directory. The packaged videos were rendered against frontend commit `e9457d32d0730396dbccf037a1c90cdfb8d348f0`; later frontend contract changes may require adapting the sample transport before regenerating them. The synthetic score is in `public/score.wav`.

## Adapt a story

`live/spec.json` defines actors, dialogue, camera and duration. `src/SceneStage.tsx` provides the room and a screen-content slot; it contains no Neige API policy. `live/state.ts` owns sample report/task data, and `live/intro.ts` drives native creation and input controls. `live/app.tsx` mounts the production application and admits only scripted sample writes and sample read receipts. `live/config.mjs` resolves the supplied frontend and reuses its Markdown runtime aliases.

`live/relay-model.ts` owns the handoff timing and flight curves. `live/RelayCompanion.tsx` renders the two partners and localized handoff flashes. Report and preview entry points remain native components. To adapt this to another product, supply that product's screen and isolated data adapter, then author a new scene and browser checks; automatic arbitrary-website filmmaking is not implemented.

Keep real project names, paths, accounts and credentials out of the sample data, generated frames and committed videos. Build output, dependency links and transient export reports are ignored.
