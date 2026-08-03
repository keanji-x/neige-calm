export const load = (transport: (url: string) => Promise<unknown>) => transport('/api');
