/** Immutable boundary points, clipped to one attached content surface. Native
 * Range objects stay local to the read: their boundaries move with the DOM. */
type Edge = 'start' | 'end' | Readonly<{ node: Node; offset: number }>;
export type ContentSelection = readonly Readonly<{ start: Edge; end: Edge }>[];

export function readContentSelection(content: HTMLElement): ContentSelection {
  const selection = content.ownerDocument.getSelection();
  if (selection === null || selection.isCollapsed) return [];
  const ranges: Array<ContentSelection[number]> = [];
  for (let index = 0; index < selection.rangeCount; index++) {
    const source = selection.getRangeAt(index);
    if (source.collapsed || !source.intersectsNode(content)) continue;
    const local = source.cloneRange();
    const startsInside = content.contains(local.startContainer);
    const endsInside = content.contains(local.endContainer);
    if (!startsInside) local.setStart(content, 0);
    if (!endsInside) local.setEnd(content, content.childNodes.length);
    if (local.collapsed) continue;
    ranges.push({
      start: startsInside ? { node: local.startContainer, offset: local.startOffset } : 'start',
      end: endsInside ? { node: local.endContainer, offset: local.endOffset } : 'end',
    });
  }
  return ranges;
}

function sameEdge(content: HTMLElement, left: Edge, right: Edge): boolean {
  if (typeof left === 'string' && typeof right === 'string') return left === right;
  const point = (edge: Edge) => typeof edge === 'string'
    ? { node: content, offset: edge === 'start' ? 0 : content.childNodes.length }
    : edge;
  const a = point(left); const b = point(right);
  return a.node === b.node && a.offset === b.offset;
}

/** Range order and selection direction do not change the selected extent. */
export function sameContentSelection(content: HTMLElement, left: ContentSelection, right: ContentSelection): boolean {
  if (left.length !== right.length) return false;
  const unmatched = [...right];
  for (const range of left) {
    const index = unmatched.findIndex(candidate => sameEdge(content, range.start, candidate.start) && sameEdge(content, range.end, candidate.end));
    if (index === -1) return false;
    unmatched.splice(index, 1);
  }
  return true;
}
