/** Parse only a complete slash name. Registration and availability belong to the caller. */
export function parseComposerCommand(text: string): Readonly<{ name: string; argument: string }> | null {
  const match = /^\/([^\s/]+)(?:\s+([\s\S]*))?$/u.exec(text.trim());
  return match === null ? null : { name: match[1], argument: (match[2] ?? '').trim() };
}
