# List typography

Shared presentation roles for the workspace sidebar and context panels.

| Role | Size | Weight | Line height | Use |
| --- | --- | --- | --- | --- |
| primary | 14px | 400 | 20px | Track and expanded card/task names |
| group | 14px | 400 | 20px | Area, status-group and conversation names |
| section | 12px | 500 | 16px | Sidebar and module headings, normal tracking |
| secondary | 12px | 400 | 16px | Status and resource kind |
| count | 12px | 400 | 16px | Right-aligned tabular numbers |

All values refer to the existing global tokens. `ListText` applies these classes without an additional DOM wrapper and supports
span, section-heading and button hosts. It preserves the caller’s attributes
and actions. The colocated CSS Module defines typography;
host components retain positioning, truncation and selection backgrounds.
Text emphasis also belongs here: `emphasis="medium"` preserves the two-line
Track row's 500 weight, and `emphasis="selected"` gives both Track and
conversation names the same primary color without changing their weight. Hosts pass state;
they do not override the name's size, family or weight in local CSS.
Tools, Tasks and Conversations are module headings. Their status groups and conversation names correspond to sidebar Areas; expanded
items correspond to Tracks. Panel entries use a 28px row cadence; sidebar Track entries use 32px,
with no extra gap between collapsed groups and a 12px surface inset. Expanded
groups keep their heading in place: no added top padding, 4px before the list
and 8px below it. Rows stay 28px apart; both insets count toward the bounded
module height. Leading icon slots never add a second
text indent to a child row. A browser contract composes the actual Sidebar,
Tools, Tasks and Conversations and checks both themes and their geometry.


`fadeOverflow` enables a measured alpha fade only when the element's text exceeds
its bounded width. Hosts retain layout/overflow ownership and use `text-overflow:
clip` instead of an ellipsis when opting in. Full DOM text, native title and
accessible names are preserved; resize and content updates remeasure overflow,
and the observer disconnects on unmount. Short labels receive no mask.
