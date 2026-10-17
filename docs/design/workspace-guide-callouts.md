# Workspace guide callouts

The guide describes existing controls. It never replaces their actions or
creates Areas, chooses directories, submits requests, or changes project files.
Ordinary mode remains unchanged. Desktop users may start it from the account
menu; an explicit `?onboarding=workspace` startup link also supports the compact
workspace navigation. Skip closes explanations and preserves the form draft.

Floating UI React owns placement, viewport flip/shift, animated-anchor updates,
reference visibility and the rounded `FloatingArrow`. A native manual popover
keeps the hint in the top layer and in its original DOM ancestry, including
inside the existing Area Dialog. There is no modal focus trap or auto-focus.
Neige tokens continue to own typography, spacing and the surface appearance.
Inside the native Area form, the hint reserves its measured height below Folder;
the form places actions on the next row while guidance is active. This prevents
help from covering Cancel or Create at compact widths. Ordinary form layout is
unchanged. Browsers without the Popover API keep the native controls and do not
activate this optional guide; the application's minimum browser is unchanged.

The shell owns the Area/defaults guide state. The Area form owns its explanatory
copy and hides the hint while its native directory chooser is presented. If
restoring a collapsed or hidden navigation preference removes the Add anchor,
the sidebar restores focus to its surviving account control.

Architecture owner decision: add `@floating-ui/react` as a direct dependency for
these optional callouts. Update only the dependency manifest and generated lock;
add no commands, gates or ownership exceptions. Record both frozen paths with
exact-path ownership trailers. The first-project readability work is tracked
by #2735. Validation covers ordinary mode, real control geometry, below/above
pointer placement, native directory selection, draft retention, focus, unsupported Popover API, and direct compact form actions.
