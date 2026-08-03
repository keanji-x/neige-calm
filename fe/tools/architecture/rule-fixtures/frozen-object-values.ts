declare const x: Record<string, unknown>;
export const entries = Object.freeze(Object.entries(x));
export const fromEntries = Object.freeze(Object.fromEntries([['a', []]]));
