declare function make(): object;
export const get = (() => { let cached: object | undefined; return () => cached ??= make(); })();
