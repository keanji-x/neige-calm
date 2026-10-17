/** Snapshot of an axis-aligned scrolling viewport, in its layout CSS pixels.
 * Rects include ancestor translation/scale; scrollTop and clientHeight do not.
 * Use the fractional CSS border-box height: offsetHeight rounds it and that
 * error grows with the distance to a target in long content.
 */
export function scrollGeometry(pane: HTMLElement) {
  const box = pane.getBoundingClientRect();
  const style = getComputedStyle(pane);
  const height = Number.parseFloat(style.height) + (style.boxSizing === 'border-box' ? 0
    : Number.parseFloat(style.paddingTop) + Number.parseFloat(style.paddingBottom)
      + Number.parseFloat(style.borderTopWidth) + Number.parseFloat(style.borderBottomWidth));
  // An unlaid-out pane has no measurable scale or scroll displacement.
  const scale = height > 0 && box.height > 0 ? box.height / height : 1;
  return {
    height: pane.clientHeight,
    y: (viewportY: number) => (viewportY - box.top) / scale - pane.clientTop,
  };
}
