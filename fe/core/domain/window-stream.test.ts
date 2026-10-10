import { describe, expect, it } from 'vitest';

import {
  FRAME_HEADER_BYTES, decodeWindowFrame, decodeWindowServerText, encodeWindowInput, toWindowPoint,
  toWindowWheel, windowStreamRetryDelay,
} from './window-stream.js';

/** A frame message laid out byte by byte as PROTOCOL.md's table says. */
function frameBytes(header: Readonly<{ version?: number; codec?: number; flags?: number; width: number; height: number }>, payload: number[]): ArrayBuffer {
  const bytes = new Uint8Array(FRAME_HEADER_BYTES + payload.length);
  bytes[0] = header.version ?? 1;
  bytes[1] = header.codec ?? 1;
  bytes[2] = header.flags ?? 1;
  bytes[3] = 0;
  new DataView(bytes.buffer).setUint32(4, header.width, true);
  new DataView(bytes.buffer).setUint32(8, header.height, true);
  bytes.set(payload, FRAME_HEADER_BYTES);
  return bytes.buffer;
}

describe('decodeWindowFrame', () => {
  it('reads the 12-byte little-endian header and keeps the payload after it', () => {
    const frame = decodeWindowFrame(frameBytes({ width: 1280, height: 800 }, [0xff, 0xd8, 0xff]));
    expect(frame).toEqual({ codec: 'jpeg', keyframe: true, width: 1280, height: 800, payload: new Uint8Array([0xff, 0xd8, 0xff]) });
  });

  it('pins the byte order against a literal header', () => {
    // 0x0500 = 1280 and 0x0320 = 800, least significant byte first.
    const literal = new Uint8Array([1, 1, 1, 0, 0x00, 0x05, 0, 0, 0x20, 0x03, 0, 0, 9]).buffer;
    expect(decodeWindowFrame(literal)).toMatchObject({ width: 1280, height: 800, payload: new Uint8Array([9]) });
  });

  it('reads the keyframe flag from bit 0', () => {
    expect(decodeWindowFrame(frameBytes({ flags: 0, width: 2, height: 2 }, [1]))?.keyframe).toBe(false);
  });

  it('ignores another version, an unknown codec, an empty size and a message with no payload', () => {
    expect(decodeWindowFrame(frameBytes({ version: 2, width: 2, height: 2 }, [1]))).toBeNull();
    expect(decodeWindowFrame(frameBytes({ codec: 2, width: 2, height: 2 }, [1]))).toBeNull();
    expect(decodeWindowFrame(frameBytes({ width: 0, height: 2 }, [1]))).toBeNull();
    expect(decodeWindowFrame(frameBytes({ width: 2, height: 0 }, [1]))).toBeNull();
    expect(decodeWindowFrame(frameBytes({ width: 2, height: 2 }, []))).toBeNull();
    expect(decodeWindowFrame(new ArrayBuffer(5))).toBeNull();
  });
});

describe('decodeWindowServerText', () => {
  it('reads the three v1 messages as PROTOCOL.md spells them', () => {
    expect(decodeWindowServerText('{"type":"hello","version":1,"codec":"jpeg","width":1280,"height":800,"title":"Example - Chrome"}'))
      .toEqual({ type: 'hello', width: 1280, height: 800, title: 'Example - Chrome' });
    expect(decodeWindowServerText('{"type":"title","title":"New title - Chrome"}')).toEqual({ type: 'title', title: 'New title - Chrome' });
    expect(decodeWindowServerText('{"type":"closed"}')).toEqual({ type: 'closed' });
  });

  it('turns a hello with another version or codec into unsupported', () => {
    expect(decodeWindowServerText('{"type":"hello","version":2,"codec":"jpeg","width":1,"height":1,"title":""}')).toEqual({ type: 'unsupported' });
    expect(decodeWindowServerText('{"type":"hello","version":1,"codec":"h264","width":1,"height":1,"title":""}')).toEqual({ type: 'unsupported' });
  });

  it('ignores unknown types, malformed fields and text that is not JSON', () => {
    expect(decodeWindowServerText('{"type":"cursor","x":1}')).toBeNull();
    expect(decodeWindowServerText('{"type":"title"}')).toBeNull();
    expect(decodeWindowServerText('{"type":"hello","version":1,"codec":"jpeg","width":0,"height":1,"title":""}')).toBeNull();
    expect(decodeWindowServerText('not json')).toBeNull();
  });
});

describe('encodeWindowInput', () => {
  it('writes the v1 input messages', () => {
    expect(encodeWindowInput({ type: 'pointer', x: 640.5, y: 400 })).toBe('{"type":"pointer","x":640.5,"y":400}');
    expect(encodeWindowInput({ type: 'button', button: 0, pressed: true })).toBe('{"type":"button","button":0,"pressed":true}');
    expect(encodeWindowInput({ type: 'wheel', dx: 0, dy: 120 })).toBe('{"type":"wheel","dx":0,"dy":120}');
    expect(encodeWindowInput({ type: 'key', code: 'KeyA', pressed: true })).toBe('{"type":"key","code":"KeyA","pressed":true}');
  });
});

describe('canvas to window pixels', () => {
  const size = { width: 1280, height: 800 };

  it('scales a point on a canvas of the window aspect', () => {
    expect(toWindowPoint(10 + 320, 20 + 200, { left: 10, top: 20, width: 640, height: 400 }, size)).toEqual({ x: 640, y: 400 });
  });

  it('removes the letterbox of a canvas wider than the window', () => {
    // 1280×800 fit into 1000×400: scale 0.5, drawn 640 wide, centred 180 px in.
    expect(toWindowPoint(180, 0, { left: 0, top: 0, width: 1000, height: 400 }, size)).toEqual({ x: 0, y: 0 });
    expect(toWindowPoint(820, 400, { left: 0, top: 0, width: 1000, height: 400 }, size)).toEqual({ x: 1280, y: 800 });
  });

  it('turns wheel lines and pages into window pixels', () => {
    const box = { left: 0, top: 0, width: 640, height: 400 };
    expect(toWindowWheel({ deltaX: 0, deltaY: 60, deltaMode: 0 }, box, size)).toEqual({ dx: 0, dy: 120 });
    expect(toWindowWheel({ deltaX: 1, deltaY: 3, deltaMode: 1 }, box, size)).toEqual({ dx: 32, dy: 96 });
    expect(toWindowWheel({ deltaX: 0, deltaY: 1, deltaMode: 2 }, box, size)).toEqual({ dx: 0, dy: 800 });
  });
});

describe('windowStreamRetryDelay', () => {
  it('doubles from half a second and stops at ten', () => {
    expect([0, 1, 2, 3, 4, 5, 6, 40].map(windowStreamRetryDelay)).toEqual([500, 1000, 2000, 4000, 8000, 10_000, 10_000, 10_000]);
  });
});
