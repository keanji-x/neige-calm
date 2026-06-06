import { useCallback } from 'react';
import type { WaveCardData } from '../types';
import type { CardInstanceCtx } from './registry';

export interface CardEntryResolverValue {
  card: WaveCardData;
  instance: Pick<CardInstanceCtx, 'cardId' | 'useInstance'>;
}

export type ResolveCardById = (
  cardId: string,
) => CardEntryResolverValue | null;

const CARD_ENTRY_RESOLVER = new Map<string, CardEntryResolverValue>();

export function registerCardEntryResolver(
  cardId: string,
  value: CardEntryResolverValue,
): () => void {
  CARD_ENTRY_RESOLVER.set(cardId, value);
  return () => {
    if (CARD_ENTRY_RESOLVER.get(cardId) === value) {
      CARD_ENTRY_RESOLVER.delete(cardId);
    }
  };
}

export function resolveCardById(cardId: string): CardEntryResolverValue | null {
  return CARD_ENTRY_RESOLVER.get(cardId) ?? null;
}

export function useCardEntryResolverRegistry(): ResolveCardById {
  return useCallback(resolveCardById, []);
}

export function __resetCardEntryResolverRegistryForTest(): void {
  CARD_ENTRY_RESOLVER.clear();
}
