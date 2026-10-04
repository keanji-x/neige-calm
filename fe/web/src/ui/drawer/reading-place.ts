// Where the reader is in the drawer's pane, taken before its width moves and put back after. The browser's scroll anchoring stands down when an ancestor's width changes, so a reflowed transcript would otherwise move under the reader.

/** What the reader is on: a character of text, or the block at the probe line. */
type ReadingMark = Range | Element;
/** Where the reader is in the pane: at its end, or on `mark`, `top` px below the pane's top. */
export type ReadingPlace = Readonly<{ atEnd: boolean; mark: ReadingMark | null; top: number }>;

/** How far below the pane's top the probe line sits: just under the header, past the body's own top padding. */
const READING_PROBE_PX = 12;

/** One character of text at the point, so a rewrapped paragraph is followed to the line being read. `?.` because jsdom has neither API. */
function characterAt(pane: HTMLElement, x: number, y: number): Range | null {
  const position = document.caretPositionFromPoint?.(x, y) ?? null;
  const caret = position === null ? document.caretRangeFromPoint?.(x, y) ?? null : null;
  const node = position?.offsetNode ?? caret?.startContainer ?? null;
  const offset = position?.offset ?? caret?.startOffset ?? 0;
  const length = node?.nodeType === Node.TEXT_NODE ? node.textContent?.length ?? 0 : 0;
  if (node === null || length === 0 || !pane.contains(node)) return null;
  const character = document.createRange();
  character.setStart(node, Math.min(offset, length - 1));
  character.setEnd(node, Math.min(offset, length - 1) + 1);
  return character;
}

/** With no text on the probe line (the gap between paragraphs, a margin), the deepest block that spans it, or the first block below it. */
function blockAt(pane: HTMLElement, y: number): Element | null {
  let box: Element = pane;
  for (;;) {
    const children = [...box.children];
    const spanning = children.find((child) => {
      const rect = child.getBoundingClientRect();
      return rect.height > 0 && rect.top <= y && rect.bottom > y;
    });
    if (spanning !== undefined) { box = spanning; continue; }
    return children.find((child) => child.getBoundingClientRect().top > y) ?? (box === pane ? null : box);
  }
}

export function readingPlaceIn(pane: HTMLElement): ReadingPlace {
  const box = pane.getBoundingClientRect();
  const y = box.top + Math.min(READING_PROBE_PX, box.height / 2);
  const mark = characterAt(pane, box.left + box.width / 2, y) ?? blockAt(pane, y);
  return {
    atEnd: pane.scrollHeight - pane.scrollTop - pane.clientHeight <= 1,
    mark,
    top: mark === null ? 0 : mark.getBoundingClientRect().top - box.top,
  };
}

const markConnected = (mark: ReadingMark) => (mark instanceof Range ? mark.startContainer.isConnected : mark.isConnected);

/** Put the reader back on `place` once the new width is laid out; reading a rect lays it out, so this runs synchronously after the width moves. */
export function restoreReadingPlace(pane: HTMLElement | null, place: ReadingPlace | null): void {
  if (place === null || pane === null) return;
  if (place.atEnd) pane.scrollTop = pane.scrollHeight;
  else if (place.mark !== null && markConnected(place.mark)) {
    pane.scrollTop += place.mark.getBoundingClientRect().top - pane.getBoundingClientRect().top - place.top;
  }
}
