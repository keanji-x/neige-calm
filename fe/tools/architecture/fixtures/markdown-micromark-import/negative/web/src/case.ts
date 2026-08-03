export const gfm = import('micromark-extension-gfm');
export const templatedGfm = import(`micromark-extension-gfm`);
export const attributedGfm = import('micromark-extension-gfm', { with: { type: 'json' } });
