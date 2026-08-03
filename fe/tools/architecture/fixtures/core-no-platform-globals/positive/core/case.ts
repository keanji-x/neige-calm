import type { WebSocket } from './types.ts';

export type Socket = WebSocket;

export const run = (fetch: () => void): void => {
  fetch();
};
