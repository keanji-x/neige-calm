import type { ChatComposerTrigger } from '@astryxdesign/core/Chat';
import { EmptyState, type EmptyStateSchema } from './public.tsx';

/** Pinned Astryx 0.6.3 renders this value as a React child; its declaration is narrower. */
export function withEmptyTrigger(trigger: Omit<ChatComposerTrigger, 'emptySearchResultsText'>, content: EmptyStateSchema): ChatComposerTrigger {
  return { ...trigger,
    // @ts-expect-error Astryx useTriggerMenu renders this directly as a React child. Real-browser contracts pin this seam.
    emptySearchResultsText: <EmptyState {...content} variant="compact" />,
  };
}
