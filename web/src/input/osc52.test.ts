import { describe, expect, it, vi } from 'vitest';
import {
  OSC52_MAX_DECODED_BYTES,
  createOsc52Handler,
  parseOsc52Payload,
} from './osc52';

function b64(text: string): string {
  return btoa(
    Array.from(new TextEncoder().encode(text), (b) =>
      String.fromCharCode(b),
    ).join(''),
  );
}

describe('parseOsc52Payload', () => {
  it('decodes a clipboard write', () => {
    expect(parseOsc52Payload(`c;${b64('hello')}`)).toEqual({
      kind: 'write',
      text: 'hello',
    });
  });

  it('decodes UTF-8 payload', () => {
    expect(parseOsc52Payload(`c;${b64('复制')}`)).toEqual({
      kind: 'write',
      text: '复制',
    });
  });

  it('treats empty Pc as clipboard', () => {
    expect(parseOsc52Payload(`;${b64('x')}`)).toEqual({
      kind: 'write',
      text: 'x',
    });
  });

  it('accepts primary and select buffers as writes', () => {
    expect(parseOsc52Payload(`p;${b64('a')}`)).toEqual({
      kind: 'write',
      text: 'a',
    });
    expect(parseOsc52Payload(`s;${b64('b')}`)).toEqual({
      kind: 'write',
      text: 'b',
    });
  });

  it('strips whitespace from wrapped base64', () => {
    const encoded = b64('wrap-me');
    const wrapped = `${encoded.slice(0, 4)}\n${encoded.slice(4)}`;
    expect(parseOsc52Payload(`c;${wrapped}`)).toEqual({
      kind: 'write',
      text: 'wrap-me',
    });
  });

  it('clears on empty payload', () => {
    expect(parseOsc52Payload('c;')).toEqual({ kind: 'clear' });
  });

  it('ignores clipboard queries', () => {
    expect(parseOsc52Payload('c;?')).toEqual({ kind: 'ignore' });
  });

  it('ignores unknown selection ids and malformed payloads', () => {
    expect(parseOsc52Payload(`q;${b64('nope')}`)).toEqual({ kind: 'ignore' });
    expect(parseOsc52Payload('c')).toEqual({ kind: 'ignore' });
    expect(parseOsc52Payload('c;!!!!')).toEqual({ kind: 'ignore' });
  });

  it('ignores oversized payloads', () => {
    const tooBig = 'A'.repeat(OSC52_MAX_DECODED_BYTES + 1);
    expect(parseOsc52Payload(`c;${btoa(tooBig)}`)).toEqual({
      kind: 'ignore',
    });
  });
});

describe('createOsc52Handler', () => {
  it('writes decoded text and always consumes the sequence', () => {
    const writeText = vi.fn();
    const handler = createOsc52Handler(writeText);
    expect(handler(`c;${b64('copied')}`)).toBe(true);
    expect(writeText).toHaveBeenCalledWith('copied');
  });

  it('clears the clipboard on empty payload', () => {
    const writeText = vi.fn();
    const handler = createOsc52Handler(writeText);
    expect(handler('c;')).toBe(true);
    expect(writeText).toHaveBeenCalledWith('');
  });

  it('consumes queries without reading the clipboard', () => {
    const writeText = vi.fn();
    const handler = createOsc52Handler(writeText);
    expect(handler('c;?')).toBe(true);
    expect(writeText).not.toHaveBeenCalled();
  });
});
