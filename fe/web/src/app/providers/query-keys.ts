import {
  initialInputPrefix, initialInputTrack, initialInput, areas,
  trackLists, tracksInArea, trackDetail, trackBacklinksPrefix,
  trackBacklinks, trackReport, trackReportPrefix, overlaysByKind,
  plugins, pluginDataPrefix, trackTemplates, todayLaunchpad,
  transcriptPrefix, transcript as harnessItems, plannerRun, modelCatalog,
  modelCatalogPrefix, trackConversations, trackConversationsPrefix,
} from '../../../../core/events/query-keys.ts';

// Key shapes match the legacy app so a cache dump reads the same in both.
export const queryKeys = Object.freeze({
  initialInputPrefix,
  initialInputTrack,
  initialInput,
  serverVersion: () => ['server-version'] as const,
  areas,
  areaFolders: (areaId: string) => ['area-folders', areaId] as const,
  trackLists,
  tracksInArea,
  trackDetail,
  trackBacklinksPrefix,
  trackBacklinks,
  /* The track's registered previews (#1780). Registrations emit no event; `trackPreviewsQueryOptions` polls. */
  trackPreviews: (trackId: string) => ['track-previews', trackId] as const,
  /* Report instances and event prefixes share the core resource address. */
  trackReport,
  /**
   * The prefix for the `task.*` events the plan cannot key by track: they carry no `track_id` field
   * (the id is only inside the opaque task id, which the plan declines to parse). A prefix invalidation
   * reaches at most the open track's report and costs nothing when none is cached.
   */
  trackReportPrefix,
  /**
   * One `chart.series` block's resolved data, keyed by the block REVISION. `rev` is the whole refresh
   * mechanism: `track.report_edited` does not name this prefix (the event carries no block id, and each
   * block is a `full` read of up to 1 MiB); a changed block arrives with a new `rev` and mounts a new key.
   */
  trackReportSeries: (trackId: string, blockId: string, rev: number) =>
    ['track-report-series', trackId, blockId, rev] as const,
  /** One captured source with its body. Not under `['track-report', …]`: the row is immutable with append-only anchors; freshness is the query's own. */
  trackSource: (trackId: string, sourceId: string) => ['track-source', trackId, sourceId] as const,
  overlaysByKind,
  settings: () => ['settings'] as const,
  /* The user's read receipts (#2596). Writes emit no event: the read refetches on focus and after each write. */
  readReceipts: () => ['read-receipts'] as const,
  /* Settings › Plugins. `plugin.state` invalidates; transient-state polling remains a supplement. */
  plugins,
  /* Plugin data changes invalidate every cached window belonging to that plugin. */
  pluginDataPrefix,
  /* One plugin's detail, read only by its configuration pane; the list carries no manifest by design. */
  pluginDetail: (id: string) => ['plugin-detail', id] as const,
  /* The New track picker: `plugin.state` changes owner availability and input schemas. */
  trackTemplates,
  /* The user's own recipes. Recipe writes emit no `Event`; the mutations below invalidate this key directly. */
  trackRecipes: () => ['track-recipes'] as const,
  /* The Today launchpad resolve. One entry, not keyed by track: `purpose = 'launchpad'` is a singleton. */
  todayLaunchpad,
  transcriptPrefix,
  harnessItems,
  /** The running turn's streamed reply text (#1923). Polled while a reply may stream; no event reaches it. */
  harnessLive: (cardId: string) => ['harness-live', cardId] as const,
  plannerRun,
  /**
   * `GET /api/models` for one scope: a card (its default resolves against that card's workspace) or a
   * provider about to be created on. Turn completion refreshes the owning card scope.
   */
  modelCatalog,
  /** Every scope's catalog: a Recheck (#1822) re-fetches the Claude CLI's list behind the Claude ones. */
  modelCatalogPrefix,
  /** `GET /api/agent-providers` (#1817). No event reaches it: the server's own 30 s cache is its freshness. */
  agentProviders: () => ['agent-providers'] as const,
  /** One track's conversation list, keyed by its track. */
  trackConversations,
  /**
   * The prefix for events that cannot name a track: the `runtime.*` kinds carry only a `card_id`, and a
   * card in a track nobody has open resolves to null. A fallback, not the house shape.
   */
  trackConversationsPrefix,
});
