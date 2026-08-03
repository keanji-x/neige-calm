import type { CardHostCapabilities } from './contracts.js';

export interface CardInstanceResolver {
  register(cardId: string, value: CardHostCapabilities): () => void;
  resolve(cardId: string): CardHostCapabilities | null;
}

export function createCardInstanceResolver(): CardInstanceResolver {
  const values = new Map<string, CardHostCapabilities>();
  return Object.freeze({
    register(cardId: string, value: CardHostCapabilities) {
      values.set(cardId, value);
      return () => {
        if (values.get(cardId) === value) values.delete(cardId);
      };
    },
    resolve: (cardId: string) => values.get(cardId) ?? null,
  });
}
