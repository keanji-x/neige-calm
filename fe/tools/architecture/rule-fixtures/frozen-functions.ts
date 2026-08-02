declare const X: Record<string, unknown>;
export const handler = Object.freeze({ handler: () => 1 });
export const adapter = Object.freeze({ run: () => 1, name: 'x' });
export const keys = Object.freeze(Object.keys(X));
