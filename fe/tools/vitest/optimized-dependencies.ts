/**
 * Dependencies pre-optimized before a dev or browser-test page starts; otherwise Vite discovers them
 * mid-run and reloads the page importing a test. Browser projects do not inherit the root list.
 */
export const OPTIMIZED_DEPENDENCIES = Object.freeze([
  '@tanstack/react-query',
  '@tanstack/react-router',
  '@astryxdesign/core/Button',
  '@astryxdesign/core/Calendar',
  '@astryxdesign/core/Card',
  '@astryxdesign/core/Chat',
  '@astryxdesign/core/Code',
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
  '@astryxdesign/core/Popover',
  '@astryxdesign/core/SegmentedControl',
  '@astryxdesign/core/TextInput',
  '@astryxdesign/core/Typeahead',
] as const);
