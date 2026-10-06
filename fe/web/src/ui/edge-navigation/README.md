# Edge navigation

`EdgeNavigator` renders a stable vertical index. Report and Chat supply item IDs,
accessible labels, preview titles and body excerpts, placement, active items and
jump handlers. This primitive imports no domain types and reads no application data.

Fine-pointer rows are 20px tall; coarse-pointer rows are 44px tall. The intrinsic
track is centered and scrolls when its content exceeds 320px. Pointer proximity smoothly enlarges neighboring ink toward 8px without changing
button geometry. One bounded frame reads all fixed row boxes, then writes only
ink sizes; scroll, resize and item changes recompute the same profile. Leaving
or cancelling the pointer resets it; touch and coarse pointers do not magnify.
Keyboard focus keeps a clear 8px dot. One roving tab stop supports Arrow, Home and End.

One Astryx `useHoverCard` owns the whole track interaction. First entry waits
180ms; moving between rows immediately updates the same card and anchor without
restarting its entrance. The 120ms leave grace lets the reader enter the card or
return to the track. Keyboard focus previews immediately; Escape and selection
close the card; touch jumps directly. Native positioning handles viewport bounds.

The card is capped at 17rem (272px at the application's root font size). The host
can further bound it with `--nc-rail-preview-max-inline-size`. `previewSide`
chooses the adjacent side: reports use the document side and conversations use
the space before the rail. State and observers belong to each mounted instance.
