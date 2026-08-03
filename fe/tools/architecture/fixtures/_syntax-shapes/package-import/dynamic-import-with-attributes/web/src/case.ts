export async function load() { return import('micromark-extension-gfm', { with: { type: 'json' } }); }
