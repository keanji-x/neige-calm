declare const existing: Map<string, string> | undefined;
export const cache = existing ?? new Map();
