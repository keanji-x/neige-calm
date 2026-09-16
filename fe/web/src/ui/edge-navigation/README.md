# Edge navigation

`EdgeNavigator` renders a dense vertical index of named items. The Report and
Chat features supply item IDs, labels, preview text and their own jump handlers.
They also own placement and determining the active item; this primitive imports
no domain types and reads no application data.

The shared behavior includes a pointer-centered magnification spread, bounded
scrolling, delayed hover and immediate keyboard-focus previews, one roving tab stop, arrow/Home/End keys and
larger touch targets. State and observers belong to each mounted instance.
The host must provide a bounded block size and a narrow inline size. It sets
`--nc-rail-preview-max-inline-size` through an ancestor or the optional `className`
to limit previews to its available space. `previewSide` selects the adjacent
side: reports use the document side, which remains readable even with a narrow
margin; conversations use the space before their rail. The primitive does not
assume a conversation layout when it is used by a report.
