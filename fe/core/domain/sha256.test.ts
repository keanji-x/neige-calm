import { describe, expect, it } from 'vitest';

import { sha256Hex } from './sha256.js';

/* Golden vectors, not round trips: a self-consistency check stays green under every arithmetic mistake. */
describe('sha256Hex', () => {
  it('matches the published vectors', () => {
    expect(sha256Hex('')).toBe('e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855');
    expect(sha256Hex('abc')).toBe('ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad');
    expect(sha256Hex('abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq'))
      .toBe('248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1');
    expect(sha256Hex('a'.repeat(1000)))
      .toBe('41edece42d63e8d9bf515a9ba6932e1c20cbc9f5a5d134645adb5db1b9737ea3');
  });

  /* 55, 56 and 64 are the lengths where `padded` decides how many blocks to make. */
  it('pads correctly at every block boundary', () => {
    expect(sha256Hex('a'.repeat(55)))
      .toBe('9f4390f8d30c2dd92ec9f095b65e2b9ae9b0a925a5258e241c9f1e910f734318');
    expect(sha256Hex('a'.repeat(56)))
      .toBe('b35439a4ac6f0948b6d6f9e3c6af0f5f590ce20f1bde7090ef7970686ec6738a');
    expect(sha256Hex('a'.repeat(64)))
      .toBe('ffe054fe7ae0cb6dc65c3af9b61d5209f439851db43d0ba5997337df154668eb');
    expect(sha256Hex('a'.repeat(119)))
      .toBe('31eba51c313a5c08226adf18d4a359cfdfd8d2e816b13f4af952f7ea6584dcfb');
  });

  /* The server hashes UTF-8 bytes; a digest over UTF-16 code units would agree on ASCII and disagree here. */
  it('hashes UTF-8 bytes, not code units', () => {
    expect(sha256Hex('héllo'))
      .toBe('3c48591d8d098a4538f5e013dfcf406e948eac4d3277b10bf614e295d6068179');
  });

  it('encodes an astral character as four bytes, not two surrogates', () => {
    expect(sha256Hex('😀'))
      .toBe('f0443a342c5ef54783a111b51ba56c938e474c32324d90c3a60c9c8e3a37e2d9');
  });

  /* A lone surrogate has no UTF-8 encoding; the claim is agreement with `TextEncoder`, which substitutes U+FFFD. */
  it('replaces a lone surrogate with U+FFFD, as TextEncoder does', () => {
    expect(sha256Hex('a\ud800b'))
      .toBe('05087813392efc16fe8ff448920c6328e53af865df39419436659d9ffda90f7b');
    expect(sha256Hex('a\ud800b')).toBe(sha256Hex('a�b'));
  });
});
