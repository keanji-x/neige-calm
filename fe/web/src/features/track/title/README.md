# Shared Track title

TrackTitle owns the displayed Track name and closed decoration. Every visible
Track title consumes it; hosts retain heading/list typography, truncation,
actions, and editable state. The domain's isClosed and trackDisplayTitle remain
the authoritative predicates. Closed does not mean completed, and does not
suppress activity indicators or the explicit Closed status.

## Interface change request and decision

The additive UI contracts approved in
[issue #1930](https://github.com/keanji-x/neige-calm/issues/1930) implement the
user-authorized shared title change:

- ui/editable-title: optional displayContent replaces only read-mode text.
  Stored value, draft, placeholder fallback, focus and commit behavior remain.
  Custom readView still owns its rendering and consumes TrackTitle directly.
- ui/menu: optional labelContent replaces only visible item text. The required
  string label remains the typeahead and accessible name.
- ui/mobile-header: optional titleText replaces text inside the existing heading.
  Custom titleContent and projection marker constraints remain distinct.
- ui/mobile-list: optional titleContent replaces only visible title text. The
  required string title remains the accessible name and projection contract.

These UI slots accept ReactNode and carry no domain policy. No global style
contract changes. The readonly EditableTitle and Menu owner changes are explicit;
commits changing those frozen paths carry their exact-path approval trailers.

Acceptance checks cover open/closed/reopened rendering, blank-name fallback,
rename without decorated input, keyboard menu selection, and desktop/mobile
production title hosts in a real browser.
