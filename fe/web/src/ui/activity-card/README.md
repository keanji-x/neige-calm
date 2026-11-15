# Activity card

`ActivityCard` composes the existing card frame with header, details, activity
and control slots. It contains no task, provider or transport policy. The caller
supplies the activity's accessible label and owns the roles, names, focus,
keyboard behavior, disabled states and lifecycle of the slotted controls.

The shell keeps long output within a scrollable region and lets the compact
input form wrap at narrow widths. It creates no focus target, event handler,
network port or animation. Native Worker browser fixtures exercise the shell
with the production card registry on desktop and mobile.
