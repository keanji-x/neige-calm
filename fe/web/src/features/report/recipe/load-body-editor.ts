/** Load the enhanced editor only for an active recipe editing session. */
export function loadRecipeBodyEditor() {
  return import('./body-editor.tsx');
}
