/**
 * SHA-256 over a UTF-8 string, as lowercase hex (FIPS 180-4).
 *
 * # Why this is not `crypto.subtle.digest`
 *
 * Two reasons, and the first one is not a preference:
 *
 * 1. `crypto.subtle` exists **only in a secure context**. This app is served
 *    over plain http on a LAN (`docker/nginx.conf` listens on 8080 with no
 *    TLS, the server binds a plain `TcpListener`, and the reader opens
 *    `http://<lan-ip>:<port>/calm/` from another machine), so
 *    `window.isSecureContext` is false and `crypto.subtle` is `undefined` —
 *    the one hashing API the platform offers is simply absent exactly where
 *    the app runs.
 *
 *    The same gate applies to `crypto.randomUUID`, which is `[SecureContext]`
 *    in the same IDL. An earlier version of this comment claimed otherwise and
 *    the draft key was minted with it; `mintIdempotencyKey` in
 *    `web/src/app/router/public.tsx` now builds the key from
 *    `crypto.getRandomValues`, which is the one member of `Crypto` that is
 *    *not* secure-context gated.
 * 2. It is asynchronous, and its only caller wants an id it can compare a list
 *    against; a promise there would make a synchronous branch async for no
 *    reason.
 *
 * Nothing here is a secret and nothing is authenticated by it: the only use is
 * recomputing a **public, deterministic** card id the server derives the same
 * way, so the client can recognise its own row in a list. A hand-written digest
 * would be the wrong answer for a signature; for "agree with the server on a
 * pure function of two strings", the golden vectors below are the whole
 * contract.
 */

const K = Object.freeze([
  0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
  0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
  0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
  0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
  0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
  0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
  0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
  0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
] as const);

const INITIAL_HASH = Object.freeze([
  0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
] as const);

function rotateRight(value: number, bits: number): number {
  return ((value >>> bits) | (value << (32 - bits))) >>> 0;
}

/**
 * UTF-8 bytes of a string, written out rather than taken from `TextEncoder`.
 *
 * `core` is the platform-independent layer and compiles with `lib: ["ES2023"]`
 * and `types: []` — `TextEncoder` belongs to the DOM and to Node, neither of
 * which is in scope here. Twenty lines of well-defined encoding is the price of
 * the layer boundary.
 *
 * Lone surrogates (an unpaired half of a surrogate pair, which a JS string may
 * legally hold) become U+FFFD, which is what `TextEncoder` does — and
 * `TextEncoder` is the encoder this hand-written one stands in for, so that is
 * the whole claim. It is **not** a claim about the server: Rust's `String`
 * guarantees valid UTF-8 and performs no such substitution.
 *
 * Nor does the question ever reach the server on this endpoint. What gets
 * hashed here is the Track-conversation namespace together with a server-minted
 * Track id and a client-minted UUID, none of which holds a surrogate. A lone
 * surrogate in the message *text* is a different path
 * entirely: the draft counts it as one code point (`Array.from(text).length`
 * against `CONVERSATION_TEXT_MAX`) and then it dies on the wire —
 * `JSON.stringify` escapes it as `\ud800`, and `serde_json` refuses a lone
 * surrogate escape when deserialising into a `String`
 * (`LoneLeadingSurrogateInHexEscape`, classified as a syntax error, so axum's
 * `Json` extractor answers 400 before `create_track_conversation` runs). The
 * server never sees one.
 */
function utf8Bytes(text: string): Uint8Array {
  const out: number[] = [];
  for (const character of text) {
    let code = character.codePointAt(0)!;
    if (code >= 0xd800 && code <= 0xdfff) code = 0xfffd;
    if (code < 0x80) out.push(code);
    else if (code < 0x800) out.push(0xc0 | (code >> 6), 0x80 | (code & 0x3f));
    else if (code < 0x10000) {
      out.push(0xe0 | (code >> 12), 0x80 | ((code >> 6) & 0x3f), 0x80 | (code & 0x3f));
    } else {
      out.push(
        0xf0 | (code >> 18), 0x80 | ((code >> 12) & 0x3f),
        0x80 | ((code >> 6) & 0x3f), 0x80 | (code & 0x3f),
      );
    }
  }
  return new Uint8Array(out);
}

/** The padded message: the bytes, a `0x80`, zeros, then the bit length as a
 *  64-bit big-endian integer — so the total is a whole number of 64-byte
 *  blocks. */
function padded(bytes: Uint8Array): Uint8Array {
  const blocks = Math.floor((bytes.length + 8) / 64) + 1;
  const out = new Uint8Array(blocks * 64);
  out.set(bytes);
  out[bytes.length] = 0x80;
  const view = new DataView(out.buffer);
  /* Two 32-bit halves rather than a BigInt: the high half is the byte length
     shifted right by 29 (×8 ÷ 2³²), which no realistic input makes non-zero,
     but writing it keeps the padding correct by construction instead of by
     assumption. */
  view.setUint32(out.length - 8, Math.floor(bytes.length / 0x20000000), false);
  view.setUint32(out.length - 4, (bytes.length << 3) >>> 0, false);
  return out;
}

export function sha256Hex(text: string): string {
  const message = padded(utf8Bytes(text));
  const view = new DataView(message.buffer);
  const hash: number[] = [...INITIAL_HASH];
  const schedule = new Uint32Array(64);
  for (let offset = 0; offset < message.length; offset += 64) {
    for (let index = 0; index < 16; index += 1) schedule[index] = view.getUint32(offset + index * 4, false);
    for (let index = 16; index < 64; index += 1) {
      const previous = schedule[index - 15];
      const ahead = schedule[index - 2];
      const s0 = (rotateRight(previous, 7) ^ rotateRight(previous, 18) ^ (previous >>> 3)) >>> 0;
      const s1 = (rotateRight(ahead, 17) ^ rotateRight(ahead, 19) ^ (ahead >>> 10)) >>> 0;
      schedule[index] = (schedule[index - 16] + s0 + schedule[index - 7] + s1) >>> 0;
    }
    let a = hash[0]; let b = hash[1]; let c = hash[2]; let d = hash[3];
    let e = hash[4]; let f = hash[5]; let g = hash[6]; let h = hash[7];
    for (let index = 0; index < 64; index += 1) {
      const S1 = (rotateRight(e, 6) ^ rotateRight(e, 11) ^ rotateRight(e, 25)) >>> 0;
      const choice = ((e & f) ^ (~e & g)) >>> 0;
      const temp1 = (h + S1 + choice + K[index] + schedule[index]) >>> 0;
      const S0 = (rotateRight(a, 2) ^ rotateRight(a, 13) ^ rotateRight(a, 22)) >>> 0;
      const majority = ((a & b) ^ (a & c) ^ (b & c)) >>> 0;
      const temp2 = (S0 + majority) >>> 0;
      h = g; g = f; f = e; e = (d + temp1) >>> 0;
      d = c; c = b; b = a; a = (temp1 + temp2) >>> 0;
    }
    const round = [a, b, c, d, e, f, g, h];
    for (let index = 0; index < 8; index += 1) hash[index] = (hash[index] + round[index]) >>> 0;
  }
  return hash.map((word) => word.toString(16).padStart(8, '0')).join('');
}
