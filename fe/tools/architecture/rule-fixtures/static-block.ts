declare const holder: { current: Map<string, string> };
export class Registry { static { holder.current = new Map(); } }
