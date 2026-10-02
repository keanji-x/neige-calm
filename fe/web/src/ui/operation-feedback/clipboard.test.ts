// @vitest-environment jsdom
import { afterEach, expect, it, vi } from 'vitest';
import { writeClipboardText } from './clipboard.ts';

afterEach(() => vi.unstubAllGlobals());
it('copies exact supplied text and exposes permission failures without moving focus', async () => {
  const writeText = vi.fn().mockResolvedValueOnce(undefined).mockRejectedValueOnce(new Error('Permission denied'));
  vi.stubGlobal('navigator', { clipboard: { writeText } });
  const text = 'Answer\n\n```ts\nx()\n```';
  await writeClipboardText(text);
  expect(writeText).toHaveBeenNthCalledWith(1, text);
  await expect(writeClipboardText(text)).rejects.toThrow('Permission denied');
  expect(document.querySelector('textarea')).toBeNull();
});
it('reports unavailable clipboard instead of pretending success', async () => {
  vi.stubGlobal('navigator', {});
  await expect(writeClipboardText('text')).rejects.toThrow('Clipboard is unavailable');
});
