# window-stream protocol, version 1

One WebSocket carries one window to one viewer. The server sends frames and
window state; the viewer sends input. `src/protocol.rs` implements this
document; its tests pin the JSON and the header bytes below.

## Session

1. The viewer opens the WebSocket. The server starts watching the window.
2. If the window is unavailable, the server sends `closed` and closes the
   socket. No frame is sent.
3. Otherwise the server sends `hello`, immediately followed by the first frame,
   which is a keyframe of the whole window.
4. The server then sends a frame whenever the window changed and the socket can
   take another message, and `title` whenever the title changed. Title changes
   are sent before the next frame.
5. When the window goes away, the server sends `closed` and closes the socket.
   `closed` is final: no frame follows it, and a frame that was waiting to be
   sent when the window went away is dropped.
6. The viewer may close the socket at any time; the server then stops watching
   the window.

**Latest wins.** The server encodes a frame only when the socket can take the
next message. Frames that change in between replace each other, so a slow
viewer sees fewer frames, never old ones, and never delays another viewer.
A viewer should draw the same way: when a frame arrives while the previous one
is still decoding, keep only the newest.

## Server → viewer

### Text messages

JSON objects with a `type` field. A viewer ignores unknown `type`s.

| `type` | Fields | Meaning |
|---|---|---|
| `hello` | `version` (number, `1`), `codec` (`"jpeg"`), `width`, `height` (window pixels), `title` (string) | First message of the session. Size the canvas to `width` × `height`. |
| `title` | `title` (string) | The window title changed. |
| `closed` | — | The window is gone. Last message; show "unavailable", not the last frame as live. |

Examples:

```json
{"type":"hello","version":1,"codec":"jpeg","width":1280,"height":800,"title":"Example - Chrome"}
{"type":"title","title":"New title - Chrome"}
{"type":"closed"}
```

### Binary messages: frames

A 12-byte header followed by the encoded payload. Integers are little-endian.

| Offset | Size | Field |
|---|---|---|
| 0 | 1 | protocol version, `1` |
| 1 | 1 | codec: `1` = JPEG |
| 2 | 1 | flags: bit 0 = keyframe (the payload decodes on its own); other bits `0` |
| 3 | 1 | reserved, `0` |
| 4 | 4 | width in window pixels (u32) |
| 8 | 4 | height in window pixels (u32) |
| 12 | … | payload |

With codec `1` the payload is one baseline JFIF image of the whole window and
every frame is a keyframe. A viewer decodes it with
`createImageBitmap(new Blob([payload], {type: "image/jpeg"}))`. The width and
height in the header are authoritative: if they differ from the canvas, resize
it. A viewer ignores frames with another version or an unknown codec.

## Viewer → server

Text messages, JSON objects with a `type` field. Coordinates and distances are
window pixels: scale CSS pixels on the canvas by `width / canvas CSS width`.
The server clamps pointer coordinates into the window and ignores pointer
messages that arrive before the first frame. It ignores and counts messages it
cannot parse, `key` codes it does not know and `button` indices it does not map.

| `type` | Fields | Meaning |
|---|---|---|
| `pointer` | `x`, `y` (numbers) | The pointer moved to (`x`, `y`). |
| `button` | `button` (`MouseEvent.button`: 0 left, 1 middle, 2 right, 3 back, 4 forward), `pressed` (bool) | A mouse button changed state at the last pointer position. Send a `pointer` first. |
| `wheel` | `dx`, `dy` (numbers) | Scroll by this many pixels; positive is right and down. Convert `WheelEvent.deltaMode` lines or pages to pixels first. |
| `key` | `code` (`KeyboardEvent.code`, e.g. `"KeyA"`, `"ShiftLeft"`), `pressed` (bool) | A key changed state. |

```json
{"type":"pointer","x":640.5,"y":400}
{"type":"button","button":0,"pressed":true}
{"type":"wheel","dx":0,"dy":120}
{"type":"key","code":"KeyA","pressed":true}
```

`code` names a physical key; the server maps it to a Linux evdev code and the
window sees a US keyboard layout. Known codes: `KeyA`–`KeyZ`, `Digit0`–`Digit9`,
`F1`–`F24`, `Minus`, `Equal`, `BracketLeft`, `BracketRight`, `Semicolon`,
`Quote`, `Backquote`, `Backslash`, `IntlBackslash`, `Comma`, `Period`, `Slash`,
`Space`, `Enter`, `Tab`, `Backspace`, `Escape`, `CapsLock`, `NumLock`,
`ScrollLock`, `PrintScreen`, `Pause`, `Insert`, `Delete`, `Home`, `End`,
`PageUp`, `PageDown`, `ArrowUp`, `ArrowDown`, `ArrowLeft`, `ArrowRight`,
`ShiftLeft`, `ShiftRight`, `ControlLeft`, `ControlRight`, `AltLeft`, `AltRight`,
`MetaLeft`, `MetaRight`, `ContextMenu`, `Numpad0`–`Numpad9`, `NumpadAdd`,
`NumpadSubtract`, `NumpadMultiply`, `NumpadDivide`, `NumpadDecimal`,
`NumpadEnter` and `NumpadEqual`.

A viewer sends key events only while its canvas has focus, calls
`preventDefault()` on them, skips `KeyboardEvent.repeat` events (the window
repeats held keys itself) and releases held keys when the canvas loses focus.

## Versioning

`hello.version` and header byte 0 carry the protocol version. A change that an
existing viewer cannot ignore (a new required message, a changed header field)
raises it. Adding a codec adds a codec id and a `codec` name; a viewer that
does not know the codec named in `hello` shows the window as unavailable.
