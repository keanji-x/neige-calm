# List typography

Shared presentation roles for the workspace sidebar and context panels.

| Role | Size | Weight | Line height | Use |
| --- | --- | --- | --- | --- |
| primary | 13px | 400 | 1.3 | Track and expanded card/task names |
| group | 13px | 500 | 1.3 | Area, status-group and conversation names |
| section | 11px | 600 | 1.3 | Module headings, uppercase with 0.06em tracking |
| secondary | 11px | 400 | 1.3 | Status and resource kind |
| count | 11px | 400 | 1.3 | Right-aligned tabular numbers |

All values refer to the existing global tokens. `ListText` applies these classes without an additional DOM wrapper and supports
span, section-heading and button hosts. It preserves the caller’s attributes
and actions. The colocated CSS Module defines typography;
host components retain positioning, truncation and selection backgrounds.
Text emphasis also belongs here: `emphasis="medium"` preserves the two-line
Track row's 500 weight, and `emphasis="selected"` gives both Track and
conversation names the same 600 weight and primary color. Hosts pass state;
they do not override the name's size, family or weight in local CSS.
Cards, Tasks and Conversations are module headings. Their status groups and conversation names correspond to sidebar Areas; expanded
items correspond to Tracks. Both use the same 28px row cadence as sidebar entries,
with no extra gap between collapsed groups and a 12px surface inset. Expanded
groups keep their heading in place: no added top padding, 4px before the list
and 8px below it. Rows stay 28px apart; both insets count toward the bounded
module height. Leading icon slots never add a second
text indent to a child row. A browser contract composes the actual Sidebar,
Cards, Tasks and Conversations and checks both themes and their geometry.
