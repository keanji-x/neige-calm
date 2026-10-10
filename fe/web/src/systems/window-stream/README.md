# Window stream viewer

`public.tsx` owns one viewer of a window-stream protocol v1 socket
(`crates/window-stream/PROTOCOL.md`); the pure codec is
`core/domain/window-stream.ts`. A consumer attaches `useWindowStream(src)`'s
`canvasRef` to one canvas and renders `status`.

- **Host:** the socket is `ws(s)://<location.host><src>`, so the page's session
  cookie and origin rules apply. `src` is the `window` block's validated
  same-origin path; this system names no plugin.
- **Lifecycle:** the socket opens when a canvas is attached and closes when it
  is detached, `src` changes, or the component unmounts. A reconnect timer and
  any in-flight decode are dropped with it.
- **Reconnect:** after `closed`, an unsupported `hello`, a socket error or a
  drop, the viewer retries from 0.5 s, doubling to 10 s; a drawn frame resets
  the backoff.
- **Live, never stale:** `status` is `live` only while the canvas shows a frame
  of the open socket. Each socket is one generation; a decode that finishes
  after its generation ended is closed unseen. Frames decode latest-wins.
- **Input:** pointer, button, wheel and key events become v1 JSON in window
  pixels, sent only while `live`. Keys go only while the canvas has focus;
  each key event's default is prevented, repeats are skipped and held keys
  are released on blur.
- **Port:** `openSocket` replaces the browser `WebSocket` for tests.
