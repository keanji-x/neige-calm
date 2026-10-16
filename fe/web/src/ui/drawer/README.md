# Drawer scroll following

`follow-scroll.ts` is a domain-free interaction primitive. A feature creates one
`ScrollFollower` per pane, supplies its bottom tolerance and consumes the
geometry-only `onAwayChange` callback to show a return control. Message identity,
arrival rules and navigation targets stay in the feature.

- `attach(pane, content)` starts following, observes both boxes and returns a
  cleanup for listeners, observers and pending input frames. A conversation
  change is a new attachment. Only one follower may own a pane.
- Wheel, touch, scrollbar and unhandled scroll-navigation keys establish reader
  intent. Nested scrollports retain their input while they can consume it or
  forbid chaining. Editing keys, cancelled defaults and zoom do not release
  following. A downward return resumes following after the input settles.
- Unbracketed focus in this content is a reading request when the target is
  outside the visible pane or the reader has left the tail. Tail controls and
  outside composers retain following; the explicit return command resumes it.
- Automatic focus restoration uses `restoreDrawerFocus(element)`, which brackets
  native `focus({preventScroll:true})` with the existing restoration fence. The
  caller knows it is restoring; the follower does not infer focus options from
  geometry. Use this entry after DOM replacement, including growth in the same
  commit. Outside a drawer it preserves the same native focus behavior.
- A newly observed non-collapsed selection in this content pauses following,
  including selection at the tail. Growth and initial attachment sample it before
  scrolling; queued `selectionchange` delivery cannot lose a race with layout.
  Each pane compares only its intersecting extent, clipped at its content edges.
  Range order and direction alone do not change that extent.
- Explicit return and a downward input return acknowledge the current selection;
  an unchanged held selection cannot cancel them. Only an observed clear/extent
  change rearms selection intent. Same-task clear/reselect may coalesce and is
  not distinguished. Automatic selection restoration uses the existing fence:
  its entrance reconciles a pending reader request, and its exit acknowledges
  the restored extent without overriding an explicit command made inside.
  Selection protection is not a browser-chrome Find hook; native Find may not
  expose its matches through DOM Selection.
- Scroll and resize report geometry. They cannot release or resume following by
  themselves. A feature calls `followGrowth()` when its tail changes; content
  resize also covers late image loading, wrapping and disclosure expansion.
- `navigate(position)` releases following before a resolved navigation writes
  the pane. Missing targets must be resolved as a no-op by the feature.
  `followToEnd()` is the explicit return command.
- `withScrollRestoration(panes, restore)` brackets a synchronous layout change
  and reading-place restoration. Following waits until the outer restoration
  ends, including exceptional exits; restored offsets carry no input provenance.

The drawer keeps its existing character/block reading marks and native browser
anchoring. No virtualizer, repeated positioning budget or message model is added.
