import { describe, expect, it } from 'vitest';

import { sha256Hex } from './sha256.js';

/*
 * Golden vectors, not round trips. A self-consistency check
 * (`sha256Hex(x) === sha256Hex(x)`) stays green under every arithmetic mistake
 * in here, and the one caller compares the result against a *server-derived*
 * id — so the only assertion worth making is against digests computed
 * elsewhere.
 *
 * The first three are the published FIPS 180-4 examples; the fourth crosses a
 * block boundary (56 bytes is the length where padding needs a second block),
 * which is the arithmetic in `padded` that no short input exercises.
 */
describe('sha256Hex', () => {
  it('matches the published vectors', () => {
    expect(sha256Hex('')).toBe('e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855');
    expect(sha256Hex('abc')).toBe('ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad');
    expect(sha256Hex('abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq'))
      .toBe('248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1');
    expect(sha256Hex('a'.repeat(1000)))
      .toBe('41edece42d63e8d9bf515a9ba6932e1c20cbc9f5a5d134645adb5db1b9737ea3');
  });

  /*
   * The three lengths where `padded` decides how many blocks to make.
   *
   * 55 is the largest input that still fits its `0x80` and its 8-byte length
   * in one block; 56 is the first that does not; 64 is a whole block that
   * needs a second one built entirely of padding. Every off-by-one in
   * `Math.floor((bytes.length + 8) / 64) + 1` shows up at one of these three
   * and nowhere else, and the vectors above happen to straddle them without
   * naming them — 'abcdbcde…' is 56, so 55 and 64 were untested.
   *
   * Digests computed with Node's `crypto.createHash('sha256')`, which is
   * OpenSSL, not this file.
   */
  it('pads correctly at every block boundary', () => {
    expect(sha256Hex('a'.repeat(55)))
      .toBe('9f4390f8d30c2dd92ec9f095b65e2b9ae9b0a925a5258e241c9f1e910f734318');
    expect(sha256Hex('a'.repeat(56)))
      .toBe('b35439a4ac6f0948b6d6f9e3c6af0f5f590ce20f1bde7090ef7970686ec6738a');
    expect(sha256Hex('a'.repeat(64)))
      .toBe('ffe054fe7ae0cb6dc65c3af9b61d5209f439851db43d0ba5997337df154668eb');
    /* One byte short of the same decision a block further along. */
    expect(sha256Hex('a'.repeat(119)))
      .toBe('31eba51c313a5c08226adf18d4a359cfdfd8d2e816b13f4af952f7ea6584dcfb');
  });

  /* The input is a string, and the server hashes UTF-8 bytes
     (`format!(...).as_bytes()`). A digest taken over UTF-16 code units would
     agree on ASCII and disagree here. */
  it('hashes UTF-8 bytes, not code units', () => {
    expect(sha256Hex('héllo'))
      .toBe('3c48591d8d098a4538f5e013dfcf406e948eac4d3277b10bf614e295d6068179');
  });

  /*
   * The four-byte branch of `utf8Bytes`, which the two- and three-byte
   * vectors above leave entirely unrun. An emoji is one code point and *two*
   * UTF-16 code units, so a loop written over `text[i]` rather than over code
   * points encodes two lone surrogates here and gets a different digest.
   */
  it('encodes an astral character as four bytes, not two surrogates', () => {
    expect(sha256Hex('😀'))
      .toBe('f0443a342c5ef54783a111b51ba56c938e474c32324d90c3a60c9c8e3a37e2d9');
  });

  /*
   * A JS string may legally hold half a surrogate pair, which has no UTF-8
   * encoding. `TextEncoder` resolves that by substituting U+FFFD and this file
   * says it does the same — so the assertion is that the lone surrogate and
   * the replacement character hash *identically*, and to the digest OpenSSL
   * gives for the replacement form.
   *
   * The server is not part of this claim: Rust's `String` does no such
   * substitution, and a lone surrogate never reaches it anyway (it would be a
   * `\ud800` escape in the JSON body, which `serde_json` rejects). What is
   * pinned here is agreement with `TextEncoder`, the encoder this file
   * replaces.
   */
  it('replaces a lone surrogate with U+FFFD, as TextEncoder does', () => {
    expect(sha256Hex('a\ud800b'))
      .toBe('05087813392efc16fe8ff448920c6328e53af865df39419436659d9ffda90f7b');
    expect(sha256Hex('a\ud800b')).toBe(sha256Hex('a�b'));
  });
});
