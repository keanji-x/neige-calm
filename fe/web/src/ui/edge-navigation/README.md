# Edge navigation

`EdgeNavigator` renders a dense vertical index of named items. The Report and
Chat features supply item IDs, labels, preview text and their own jump handlers.
They also own placement and determining the active item; this primitive imports
no domain types and reads no application data.

The shared behavior includes a pointer-centered magnification spread, bounded
scrolling, a delayed hover preview, one roving tab stop, arrow/Home/End keys and
larger touch targets. State and observers belong to each mounted instance.
The host must provide a bounded block size and a narrow inline size.
