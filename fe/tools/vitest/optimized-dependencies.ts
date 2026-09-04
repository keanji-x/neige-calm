/**
 * Dependencies that must be present before a dev or browser-test page starts.
 *
 * Vite otherwise discovers these late imports mid-run, rebuilds its optimizer
 * graph, and reloads the page that is importing a test. Browser projects are
 * isolated configs and do not inherit the root list, so every consumer imports
 * this one frozen roster instead of maintaining a partial copy.
 */
export const OPTIMIZED_DEPENDENCIES = Object.freeze([
  '@tanstack/react-query',
  '@tanstack/react-router',
  '@astryxdesign/core/Button',
  '@astryxdesign/core/Calendar',
  '@astryxdesign/core/Card',
  '@astryxdesign/core/Chat',
  '@astryxdesign/core/Collapsible',
  '@astryxdesign/core/Divider',
  '@astryxdesign/core/Heading',
  '@astryxdesign/core/Icon',
  '@astryxdesign/core/IconButton',
  '@astryxdesign/core/List',
  '@astryxdesign/core/Markdown',
  '@astryxdesign/core/MetadataList',
  '@astryxdesign/core/MoreMenu',
  '@astryxdesign/core/NumberInput',
  '@astryxdesign/core/SegmentedControl',
  '@astryxdesign/core/TextInput',
  '@astryxdesign/core/Typeahead',
] as const);
