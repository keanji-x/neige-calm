import type { ModelCatalogScope } from '../domain/conversation.ts';

/**
 * Cache addresses for resources reachable by server events. Plans and query registrations
 * share these factories; prefix factories address every cached instance of a resource.
 * Pure values only: executing cache operations remains the platform adapter's responsibility.
 */
export const initialInputPrefix = () => ['conversation-initial-input'] as const;
export const initialInputTrack = (trackId: string) => ['conversation-initial-input', trackId] as const;
export const initialInput = (trackId: string, cardId: string) => ['conversation-initial-input', trackId, cardId] as const;
export const areas = () => ['areas'] as const;
export const trackLists = () => ['tracks'] as const;
export const tracksInArea = (areaId: string) => ['tracks', areaId] as const;
export const trackDetail = (trackId: string) => ['track', trackId] as const;
export const trackBacklinksPrefix = () => ['track-backlinks'] as const;
export const trackBacklinks = (trackId: string) => ['track-backlinks', trackId] as const;
export const trackReport = (trackId: string) => ['track-report', trackId] as const;
export const trackReportPrefix = () => ['track-report'] as const;
export const overlaysByKind = (entityKind: 'track' | 'card') => ['overlays', entityKind] as const;
export const plugins = () => ['plugins'] as const;
export const pluginDataPrefix = (id: string) => ['plugin-data', id] as const;
export const trackTemplates = () => ['track-templates'] as const;
export const transcriptPrefix = () => ['harness-items'] as const;
export const transcript = (cardId: string) => ['harness-items', cardId] as const;
export const plannerRun = (cardId: string) => ['planner-run', cardId] as const;
export const modelCatalog = (scope: ModelCatalogScope) => ['model-catalog', scope] as const;
export const modelCatalogPrefix = () => ['model-catalog'] as const;
export const trackConversations = (trackId: string) => ['track-conversations', trackId] as const;
export const trackConversationsPrefix = () => ['track-conversations'] as const;
