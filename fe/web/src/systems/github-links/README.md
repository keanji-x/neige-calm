# GitHub link previews

`public.tsx` owns GitHub citation parsing, hover presentation and lazy query lifecycle.
The app supplies `GitHubPreviewPort` through `GitHubPreviewProvider` within its session
and QueryClient providers. Reads retain the existing unauthorized/recovery checks.
Queries share summaries for one minute and do not run before opening a card.

Chat preserves link navigation; report prose reuses its existing preview controls,
substituting summaries for GitHub iframe content. Compact and coarse-pointer clients retain their
previous rendering with no card or read. HoverPreview owns positioning, pointer travel,
focus and Escape dismissal. Summaries remain plain text; no GitHub content is executed.
