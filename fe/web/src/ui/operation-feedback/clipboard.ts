/** User-initiated browser clipboard write. Rejections stay explicit; no selection/focus fallback. */
export function writeClipboardText(text: string): Promise<void> {
  if (typeof navigator === 'undefined' || typeof navigator.clipboard?.writeText !== 'function') {
    return Promise.reject(new Error('Clipboard is unavailable in this browser.'));
  }
  return navigator.clipboard.writeText(text);
}
