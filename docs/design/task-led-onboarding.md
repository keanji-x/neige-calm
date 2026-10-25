# Guide a first task, rather than catalogue controls

Outcome: an enrolled beginner can follow a concrete local page exercise from
creating a project to writing a request, inspecting its result and asking for a
revision. Each callout gives an action and its expected visible result.

Retain the native UI, optional enrollment, Skip and replay. Split the new Area
form into name, working method, directory and creation steps. Provide explicit
example insertion for an empty project name and an empty new-track composer;
the owning feature updates its existing draft, and the user still presses the
real submit button. Never replace a nonempty draft, submit a task from a guide,
or pick a directory/workflow on the reader's behalf.

The JSON catalogue owns plain-text actions, outcomes and examples. The generic
guide renders them and invokes only an optional callback registered by the
target owner. It does not know project identities or mutate application state.
Example requests first inspect the directory and decline to overwrite existing
files. Existing projects remain opt-in, and the initial workspace tour stops
advertising secondary settings before the first task.

Review tier: L1; the existing persistence and authorization contracts remain
unchanged. Verify actual draft insertion and no implicit submission, preserving
typed text and Skip, native menus/pickers, desktop and narrow-screen placement,
and the mounted application path. Re-run frontend lint/build and text gates;
review the focused delta independently before updating the preview.
