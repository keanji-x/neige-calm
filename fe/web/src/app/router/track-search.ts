export type MobilePanel = 'outline' | 'cards' | 'tasks' | 'conversations';
export type TrackSource = 'pages' | 'area';

/** The whitelisted track query string, as the router validates and rebuilds it. */
export type TrackSearch = Readonly<{
  card?: string;
  file?: string;
  panel?: MobilePanel;
  from?: TrackSource;
}>;
