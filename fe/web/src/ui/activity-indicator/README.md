# Activity indicator

A domain-free visual marker: working spins grey, unread is accent, attention is
warn, failed is error, and `quiet` renders nothing. Motion follows the
reduced-motion preference.
The owning feature decides state precedence and supplies an accessible name or
description on its control. The marker itself is decorative.

`spoken` is the exception, for a surface where no owning control names the
fact: a card or task row (its status word says which phase this is), a
terminal card head (its words say how the connection stands). Pass the
feature's own vocabulary — `activityLabelOf(state)` from
`core/domain/activity.ts`, which this primitive may not import — and it is
rendered visually hidden right after the marker; `data-nc-activity` stays on
the visual span only. Do not pass it where a row's name or description
already carries the fact (rail rows, Today rows, conversation rows) or where
one spoken fact per region is already said elsewhere (the conversation
thread's marks).
