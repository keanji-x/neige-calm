# `features/settings`

Workspace settings. Every surface here is presentational: it never calls an API.
The bag, the in-flight flag, the error strings and the callbacks all arrive as
props; `app/shell/settings-overlay` owns fetching and mutating.

## The standard

**One nav entry per group. One pane per entry. One row shape.**

A settings group that wants a heading is a nav entry — not a block stacked on a
pane. General therefore holds workspace-wide defaults, while Network,
Appearance and About remain their own entries instead of becoming headed blocks
inside it.

A row is:

```
title                                                            [ control ]
one sentence
```

Left-aligned text, right-aligned control, both flush with the pane's two edges,
a hairline between rows and nothing else. Every control is the same width
(`CONTROL_WIDTH`), so a pane has exactly one trailing edge whatever sits on it —
a text field, a dropdown, a toggle, a chevron.

That row is `SettingRow`, and it is the **only** way to put anything on a pane.
A row is either something you set (`control`) or somewhere you go (`onOpen`),
never both — the props type makes the pair unrepresentable, and astryx's list
guidance rejects an interactive control inside an interactive row for the same
reason: two targets for one intent.

### Hierarchy

Three levels, and only three:

1. the dialog title — `Settings`
2. the pane — a heading plus **one sentence** saying what the group is for
3. the rows

The lede is required by the props. A group that cannot be said in one sentence
is two groups, and the nav column is where the second one goes.

### Choosing a control

| Kind of setting | Control |
| --- | --- |
| free text | `TextInput`, `isLabelHidden` (the row's title is the label) — **commits on blur / Enter** |
| positive integer | `NumberInput`, `isLabelHidden` — **commits on blur / Enter** |
| one of several | `Selector` — a dropdown that states the current value |
| on / off | `Switch` |
| opens a screen | nothing: `onOpen`, and the row ends in a chevron |
| read-only fact | the value as plain text |

Theme is a `Selector` and **not** a segmented control: three fixed segments
spend the row's whole trailing edge showing two options nobody picked, and they
cannot grow — a fourth theme would have to change the control.

### Icons

Astryx's built-in registry only. That set has 26 semantic names and none of them
is "network" or "appearance", so each nav entry takes the nearest available
sense and says so where it is chosen (`externalLink` for traffic leaving the
machine, `menu` for General's short list, `viewColumns` for how the app is
painted, `wrench` for tooling, `info` for read-only facts). The app deliberately
does not draw one-off glyphs for this.

## Sections and routes

| Route | Section | Component |
| --- | --- | --- |
| `/settings` | General | `GeneralPane` (`public.tsx`) |
| `/settings/network` | Network | `NetworkPane` (`public.tsx`) |
| `/settings/appearance` | Appearance | `AppearancePane` (`public.tsx`) |
| `/settings/plugins` | Plugins | `PluginsPane` (`plugins.tsx`) |
| `/settings/plugins` (2nd level) | one plugin's configuration | `PluginConfigPane` (`plugin-config.tsx`) |
| `/settings/plugins` (2nd level) | the install form | `PluginAddPane` (`plugin-add.tsx`) |
| `/settings/about` | About | `AboutPane` (`public.tsx`) |

Real routes rather than pane-local state: every pane can be linked to, and Back
leaves the pane rather than leaving Settings. (#1230 added a two-level Templates
section here — a list and a per-template editor — which is why the rule is
stated in terms of levels at all. #1300 S1 removed both; see "Templates are read
only" below.) The dialog around
them is owned by `app/shell` so it survives navigation between its own sections
— see `app/shell/settings-overlay.tsx`.

**Going back from a second level** is a `‹ Parent` ghost button above the title,
never a filled button beside it: a filled button beside a title reads as an
action *on* the thing, not as the way back out of it.

**The overlay is top-aligned, not centred.** `.dialog-overlay-wide` hard-codes
`align-items: start` and lives in `styles/`, which is frozen; changing it needs
an `OWNERSHIP-CHANGE` trailer against an issue. The pane's `min-block-size`
floor is the mitigation available from this layer, and it is not centring.

## Plugin configuration is the one two-level section (#1284 S4)

A plugin row whose kernel-supplied `has_config` is true carries a `Configure`
button beside its switch; a row without it carries **no entry point at all**.
That absence is the contract: "this plugin has nothing to configure" and "the
configuration screen is not built" have to be two different things on screen,
and a Configure button that opens an empty pane is how they became one thing.
The bit is on the list row because the list carries no manifest and never will,
so nothing else on that screen can decide the question.

The row therefore ends in two controls rather than a chevron. The row grammar
forbids a *clickable row* that also holds a control — two targets for one
intent — and offering the drill-in as the row would have taken the switch away.
Two adjacent, separately named controls are two intents with one target each.

Which plugin is open is **visit state, not a route**, and it is the one place
this domain departs from "the URL is the state". The level holds an operator's
unsaved edits against a schema version the kernel may have replaced since, so a
shareable link to it would arrive either with somebody else's draft or empty on
a plugin that no longer declares the field the link was made for.

The form is rendered from `detail.config_schema` — the **top-level** field, and
never `detail.manifest.config_schema`. The manifest blob is published verbatim
as the row it is, so on a kernel that predates #1284 it simply has no such key;
the top-level copy comes off the registry, which is what the write path
validates against.

### Its Save button, and why it is not an exception to the rule below

`configPatchFrom` may only send the keys the operator edited (#1284 §2.2.5): the
kernel applies manifest defaults on read and never stores them, so a
self-committing row would write a default into the row the moment the reader
tabbed through a field — and a manifest that later changed that default could
never reach the plugin again. Nothing takes effect until the plugin restarts
either, so a per-row commit would confirm a save that changes nothing
observable. Hence one explicit **Save**, and one explicit **Apply & restart**
that is the only control on the screen which makes a configuration live.

The three ways a restart ends are `core/domain/plugins`'s `reloadOutcome`, and
the pane renders its verdict rather than classifying anything: a reload stops
the plugin before re-reading anything, so the status code cannot tell "the lock
was held and nothing happened" from "it is down with a reason in `last_error`".
`unavailable` is a connector's normal terminal state and is painted in warn
tones, never error tones.

## Adding and removing a plugin (#1480)

The list ends in an **Add a plugin** drill-in row, and that row is also the only
thing on the empty list besides its sentence: a screen that says "none" and
offers no way out of it makes the reader go looking elsewhere for its single
next step.

The form behind it is a second level for the same reason the configuration pane
is one — it holds what the operator is typing, including a credential — and it
is visit state rather than a route for the same reason too.

It offers two sources, because they are not two ways to do one thing. A **remote
MCP server** is described in the form (name, id, URL, key) and the kernel writes
the plugin tree itself, credential included, into a `secrets.json` it keeps
`0600`. A **server directory** installs a tree that already exists on the machine
the workspace runs on — the only way to install a plugin that runs code — which
is why it asks for a path and not for a file: a picker would read the operator's
own computer, which is not where the plugin has to be.

The form asks which **tools** to expose, and refuses an empty list.
`mcp_http.tools_allow` is a strict allowlist — the kernel materializes exactly
the names it holds — so a connector installed with none comes up `running` and
contributes nothing to any conversation, which is the one failure on this screen
that looks like success. "Empty means all" is deliberately *not* invented here:
the allowlist exists so an upstream cannot add a tool behind the operator's
back.

The **API key is typed once**. It is a password field, it goes out with the
install request, and nothing here can show it again — so this screen offers no
way to *edit* a stored key; re-adding the plugin is how a key is replaced.

### Remove asks first, in the row

Every row carries a **Remove** button, and pressing it does not remove anything:
it replaces the row's other controls with `Remove <name>` / `Keep <name>` and
puts the question under the plugin's name. Three things about that shape are
deliberate:

* the question **replaces** the switch rather than sitting beside it, so a
  destructive answer and an ordinary one never share a pointer — and the row
  stays at two controls, which is what the grammar costs to remain legible;
* both buttons **name the plugin**, because the confirm is precisely the moment
  the reader has to know which row they are answering for;
* the sentence says what removal *costs* — the stored configuration, and a
  remote server's saved key — since the switch beside it already says that
  turning a plugin off loses nothing, and this is the act that is not that.

One row asks at a time. Two half-finished removals are two things the reader has
to keep apart, and nothing is gained by allowing them.

### The row's live region is located by `data-nc-effect-boundary`

astryx renders a visually-hidden `role="status"` region inside every `Button`
for its loading announcement, so once the row grew a Remove button "the status
region in this row" stopped being a unique description. The effect-boundary line
carries a `data-nc-` locator, exactly as the controls cluster does, and its role
is asserted on the element the locator finds rather than being used to find it.

## A setting has no Save button (INV-SETTINGS-003)

A row commits itself. There is no Save and no Reset on a settings pane: a proxy
is one value, and asking the reader to press Save for one value is asking them
to do the app's bookkeeping. (This is a rule about **a setting**. A plugin's
configuration is a multi-key document with a restart between storing it and
using it — see the section above for why that makes a Save structural rather
than a matter of taste.)

**Commit on blur and Enter, never per keystroke.** A half-typed URL is not a
value — saving on change would `PUT` `h`, `ht`, `htt`… and leave whatever the
reader stopped at as the workspace's proxy if they walked away mid-word.
Leaving the field is the moment the value is finished; Enter is the same intent
stated explicitly. Both are covered, and a mutation that moves the commit to
`onChange` turns *"does not write while the reader is still typing"* red.

**A field that was not edited writes nothing.** Focusing and leaving a row must
not `PUT`; the guard compares against the last value the server gave us, and
removing it turns *"writes nothing when a field is entered and left untouched"*
red.

**The confirmation is a tick, and only a tick.** "Saved." beside a green mark is
the mark said twice, and the word costs the row a line that reflows every row
under it each time you leave a field. The word is not gone, though — it moves to
a visually-hidden live region beside the field, which is what a screen reader
announces and what the tests and the e2e test locate. That region is mounted
**always**, empty, one per row: a live region that arrives in the same mutation
as its text is commonly not announced at all. Two rows therefore hold two
regions, so anything locating one has to scope to its row. A tick with no accessible
name would make the confirmation sighted-only. A *failure* keeps its sentence:
"something went wrong" is not a thing a mark can say.

**The confirmation and the failure land on the row that committed**, never as a
pane-level banner: with two proxy rows, a pane-level message cannot say which
one it is about. `saveError` keeps the typed value in place — a failed write
must not also lose the text.

**An example is painted lighter than a value.** A placeholder is a suggestion,
and astryx's own placeholder tone is one step off body text, which read as a
setting somebody had made. Settings rows drop it to the lightest text token —
the only step in the scale wide enough to say "not your value" without a second
channel (italics, quotes) doing the saying.

Both are claims about *paint*, so both are held in the browser tier
(`mobile.browser.test.tsx`): jsdom reports every element as visible and declines
to compute `::placeholder`, so neither is falsifiable there.

## `null` clears a key (INV-SETTINGS-001)

A field the user blanked is sent as `null`, **never `''`**. `core/domain/settings`
notes that the kernel deletes a key for either value, so the two converge on the
same state — sending `null` states the intent instead of relying on that
equivalence, and keeps the wire shape honest if the kernel ever stops treating
`''` as a delete.

A commit carries **one** key — the row's own. A save therefore never rewrites a
value nobody edited, so two tabs editing different keys cannot clobber each
other.

Re-seeding compares the incoming bag **by value**, not by object identity: a
parent that hands back a fresh object every render must not wipe out what the
user is typing. A genuine server-side change does re-seed.

## Appearance is deliberately not server-persisted

Theme is a per-device preference, so it never goes through `onSave` and never
enters the settings bag. `ThemeMode` is declared here rather than imported from
`app/theme` because `features/**` may not import `app/**`; the app layer adapts
to this union at the router seam.

## Loading is not an empty form (INV-SETTINGS-002)

While `settings === undefined` the Network pane renders a loading line and
**no text input at all**. An empty form would let the user save blanks over real
values before the bag has landed.

## Templates are read only (#1300 S1)

There is **no Templates section here**. #1230 added one — a list plus a
per-template editor writing through `PUT /api/track-templates/{id}` — and #1300
removed both.

The editor had no storage of its own. A template was a hidden track in the system
area, so "save a template" was an ordinary track-report write, and #1300 removes
that hidden track because it is the last production path on which the kernel
writes a report as `EditAuthor::User`. The editor went with its storage.

Its ceiling is worth recording, because it is what a replacement has to beat:
the tasks lived in a track report, so `track_report_edit_guard` (#1179) governed
them — a task `key` was immutable for the life of its block, and a live task
could only leave as a tombstone that every later fork then copied. So the editor
could reword and append, never rename, delete or reorder. Making templates
editable again needs its own persistence model and version semantics, not a track
borrowed as template storage.

Templates are still **listed**: `GET /api/track-templates` feeds the New track
picker, and that read is unchanged.

## Accessibility contract

- Errors render in `role="alert"`; `savedAt` drives a transient `Saved.` in
  `role="status"`.
- **Intentionally not done:** no `<a href>` anywhere (INV-A11Y-061).

## Test contract

`getByRole` / `getByLabelText` only — never a CSS class selector.
`public.test.tsx` holds behavior; `public.contract.test.tsx` holds invariants —
including the section list, which is where the removed Templates entry is
asserted absent.
