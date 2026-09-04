// The new-track page: one thing to say, and two optional chips under it saying
// what it is carried out on.
//
// Presentational + local form state — it never calls an API. The caller owns
// `POST /api/tracks`, `submitting`, `error`, and the template list itself —
// including putting the sentence on that create: see "Where the sentence goes".
//
// `area_id` is not a field. The page is `/area/{id}/new`; the route already
// knows which area and sends it.
//
// ## Why this is a page and not a dialog (#1211)
//
// It used to be a modal over whatever you were looking at. A modal was the
// wrong container for the *only* thing this surface does: it cannot be linked,
// it does not survive a refresh, it has no Back, and it asks the reader to
// finish or discard before they may look at anything else — which is exactly
// backwards for a screen whose two settings are things you might want to go
// check on. A route has all four properties for free.
//
// It also makes the composer the subject of a page rather than a control in a
// box, which is what the shape is: one centred field, and the page is
// otherwise empty on purpose.
//
// ## Why this is a composer and not a form (#1211)
//
// The field this replaced was the track's `title`, and it was doing two jobs at
// once: naming the track, and being the one place the user ever said what they
// wanted. #1211 split them — the kernel now accepts a create with no title at
// all and the planner agent names the track itself through `calm.track.rename` — so
// what is left to collect here is the *intent*, and intent is a sentence, not
// a label.
//
// That changes the shape of the surface rather than just its wording. A form
// asks you to fill in fields; you have to know what each one wants before you
// can start. A composer asks you to say something, which is the only thing
// this product ever asks anywhere else — the track page's planner drawer is a
// composer, and Track conversations use a composer. Creating a track was the one
// place with a different grammar, and there was no reason for it.
//
// So: `ChatComposer` from astryx, the same component the chat thread uses, with
// the two settings as footer chips. There is no Cancel button — the way out of
// a page is Back, and inventing a second one here would be a button that means
// "Back" but does not update history. And there is no separate "Create track"
// step: the send button *is* the create, and Enter reaches it.
//
// ## Where the sentence goes (#1299)
//
// **Not** into `title`: the draft carries it as `message`, and the caller
// creates the track with no title at all.
//
// Its destination is the new track's planner agent, as the first message, and
// it gets there on the create itself — `first_message` on `POST /api/tracks`,
// seeded inside the same transaction that starts the harness. This form does
// not deliver it and must not learn to: two review rounds showed the three-write
// sequence a component would need cannot be made sound (see `NewTrackRoute`),
// which is why the kernel took the write. All this form does is hand the
// sentence to its caller — **verbatim**, exactly as it was typed.
//
// Whitespace is load-bearing here in two different ways, and the two must not
// be confused. Whether the draft is blank decides whether Create is live at
// all, and that question is asked with `isBlankForKernel` — the kernel's own
// Unicode criterion, so this form never enables a send the server is bound to
// answer 400. What is *submitted* is the untouched string: the kernel stores
// and forwards it untrimmed, so stripping the reader's indentation on the way
// out would deliver something they did not write.
//
// There is nothing on screen about repeating yourself: the sentence arrives in
// the track's planner conversation on its own.
//
// ## The folder picker needs a `Dialog` above it
//
// `DirectoryField` has two modes and picks between them by asking
// `useDialogView()` whether a dialog is above it: inside one it *pushes a child
// view* onto that dialog, and outside one it falls back to rendering
// `DirectoryBrowser` **inline, in the page**. On a route there is no dialog, so
// the fallback is what fires — and a file browser unrolling underneath a chip
// is not a picker, it is the page growing a second screen's worth of list.
//
// The child-view route is not open to this page either: `DialogViewContext` is
// private to `ui/dialog`, so a surface cannot host pushed views without being a
// `Dialog`, and the chip has to render *outside* whatever modal the picker
// lives in — it is one of the composer's footer controls.
//
// So this page does not use `DirectoryField` at all. It renders its own chip
// and puts `DirectoryBrowser` — the same `ui/` primitive the field wraps —
// inside its own `Dialog`. That is a real modal picker, and it also dissolves a
// naming problem the field could not express: `DirectoryField` builds its
// accessible name as `${placeholder}: ${path}`, so a chip whose empty text is
// the *default* ("Neige workspace") would read "Neige workspace: /srv/app" once
// a folder was picked — the default's name glued to the value that replaced it.
// Owning the chip means the empty label and the purpose phrase can be two
// different strings, which is what they are.
//
// ## The folder is optional and starts from the Area preference (#1147 S3)
//
// A saved Area folder preselects that exact repository. With no Area folder —
// or after the reader explicitly clears it — the draft carries no `cwd` and the
// caller's POST omits `cwd` / `attach_folder`, which is the kernel's *managed*
// workspace branch: it allocates a directory under the workspace root, `git
// init`s it, and owns it. A filled value attaches the Track to a repository the
// user already has, which the kernel never creates, moves or deletes. Create
// time is the only UI entry into that choice.
//
// ## The template (#1209)
//
// "No template" is a first-class option when the Area has no saved template,
// or when the reader explicitly clears that preference for this Track. It is
// **not** a row the server sent: it is the absence of `template_id` on create.
// `templates` may be empty because the read failed or has not landed, and the
// composer remains usable when no unresolved Area default must be preserved.
//
// The words on screen are the reader's, not the codebase's. Both chips name
// their current **default** rather than asking a question. When the Area has no
// preference those are "No template" and "Neige workspace"; otherwise leaving
// them alone keeps the Area's template/folder. `NO_STARTING_POINT` carries the
// absence value; the shared starting-point and folder pills own the labels.
//
// One concept, one word, one field: the list, the chip and the wire all say
// *template* / `template_id`. (#1209 removed the vocabulary seam this comment
// used to describe, where the read side and the write side used different
// words for the same thing.)
//
// ## Two kinds in one list (#1292)
//
// The same chip now also offers the reader's own **recipes**. They are two
// server resources — `GET /api/track-templates` and `GET /api/track-recipes` —
// with no combined endpoint and no discriminator field on either payload; the
// kind *is* which endpoint answered, and this file is where the two are tagged
// and merged. See `StartingPoint` for why the selection had to stop being a
// bare id string; the shared starting-point pill adds band headings only when
// both kinds are present.
//
// **Duplicating a built-in as a recipe is not offered**, and that is a
// deliberate omission rather than an oversight: `GET /api/track-templates`
// returns structured `tasks[]` and never a Markdown `body`, so producing a
// recipe from a template client-side would mean re-implementing the kernel's
// `render_fence` in TypeScript — a second fence writer, which is exactly the
// duplication #1300 spent a slice removing. It belongs on the server if it is
// ever wanted.
//
// ## Mobile: declared, not inherited
//
// **This page has no mobile entry point today.** The only way in is the
// sidebar's per-area `+`, and the sidebar is not rendered below
// `@media (width < 60rem)` — so nothing here, including the two-group picker,
// is reachable on a phone. That is a pre-existing divergence this slice does
// not widen and does not fix; it is written down because the project's rule is
// that mobile may be partial as long as the difference is declared.
//
// ### Collapsed, not spread out
//
// `DropdownMenu` and not `Selector`, `Popover` or `CommandPalette` — the
// reason is where DOM focus goes, and it decides the hover card below:
//
//   * `Selector` is the semantically nicer control (`role="listbox"` +
//     `aria-selected`, which is exactly "one of N"), but it drives its list
//     with `aria-activedescendant`: DOM focus never leaves the trigger button
//     (`Selector.tsx` keeps `triggerRef` focused and only sets
//     `aria-activedescendant`). An option therefore never receives `focusin`,
//     so a per-option hover card would be mouse-only. It also renders
//     `role="combobox"`.
//   * `DropdownMenu` navigates by *moving focus*: `useListFocus.focusIndex`
//     calls `target.focus()` on the `[role="menuitem"]` element. That is what
//     makes a hover card attached to the option itself reachable by keyboard.
//     Its items are `tabIndex={-1}`, so the whole control is one tab stop.
//   * `Popover` is an empty surface — using it means hand-rolling the list,
//     its roles and its keyboard model, which is what astryx is here to avoid.
//   * `CommandPalette` is a modal search dialog. A second modal inside this
//     one, with a search box, for three options.
//   * `Selector` **with `renderOption`** — `Selector` takes
//     `renderOption?: (option: SelectorOptionData) => ReactNode` and
//     `SelectorOption` takes `description?: ReactNode`, so the template's task
//     keys could be a one-line description inside `role="option"` with no hover
//     card at all. Not taken here: the description would land inside the
//     option's accessible name (an option would read "Small change inspect,
//     implement, verify"), and the multi-line goal text the card shows does not
//     fit one line. Recorded so the next reader does not conclude that astryx
//     has no listbox answer — it does, and it is the cheaper one if the content
//     is ever cut down to keys.
//
// The one thing `DropdownMenu` cannot express is *which* item is chosen:
// `DropdownMenuItem` hard-codes `role="menuitem"` and offers no
// `menuitemradio`/`aria-checked`. Two things stand in for it, and both are
// asserted: the trigger's accessible name is "Template: <current choice>" —
// the shared pill gives Astryx's Button that full `label` while rendering the
// bare choice through `children`, so the popup inherits the same useful name —
// and the chosen item carries a check icon plus a hidden "Selected".
//
// ### One astryx limit this shape runs into, measured and left standing
//
// It lives inside `@astryxdesign/core`, so it is written down rather than
// worked around with a local fork.
//
// **The hover card's `role="dialog"` is a DOM descendant of the
//     `role="menu"`.** `HoverCard` renders its layer inline next to the
//     trigger — deliberately, "no portal is needed" (`HoverCard.tsx`). The
//     trigger here is a menu item, so the layer is emitted inside the menu, and
//     a `menu`'s owned children are supposed to be `menuitem`s only. In
//     Chromium the computed tree is very likely still correct — the intervening
//     wrapper is `display: contents` with no role, and the popover is in the top
//     layer — but that is astryx's rendering detail carrying the ARIA
//     structure, not something this file guarantees.

import { useEffect, useRef, useId } from 'react';
import { Banner } from '@astryxdesign/core/Banner';
import { Button } from '@astryxdesign/core/Button';
import { ChatComposer, ChatComposerInput } from '@astryxdesign/core/Chat';
import { CheckboxInput } from '@astryxdesign/core/CheckboxInput';
import { HStack } from '@astryxdesign/core/HStack';
import { Icon } from '@astryxdesign/core/Icon';
import { TextInput } from '@astryxdesign/core/TextInput';
import { VStack } from '@astryxdesign/core/VStack';

import { parseGitHubIssueUrl } from '../../../../../core/domain/issue-url.ts';
import { isBlankForKernel, type TrackRecipe, type TrackTemplate } from '../../../../../core/domain/track.ts';
import type { ListDirectory } from '../../../ui/directory-browser/public.tsx';
import { DirectoryBrowser } from '../../../ui/directory-browser/public.tsx';
import { Dialog } from '../../../ui/dialog/public.tsx';
import { useState } from '../../../ui/state/public.ts';
import {
  FolderPill, NO_STARTING_POINT, StartingPointPill, type StartingPoint,
} from '../default-pills/public.tsx';
import styles from './new-track.module.css';

/**
 * The starting point the draft carries, as a union rather than two independent
 * optional keys (#1292).
 *
 * `template_id` and `recipe_id` are mutually exclusive on the wire — the
 * kernel answers a request naming both with a 400 — and two optional string
 * fields say nothing about that: `{template_id, recipe_id}` type-checks
 * perfectly and is a 400. Written as three arms with the other keys pinned to
 * `undefined`, the exclusivity is a property of the type, so a draft carrying
 * both does not compile. This is the same trick `StartingPoint` uses for the
 * selection this is built from, one screen away.
 *
 * Every arm names every key, which is what lets the caller keep reading
 * `draft.template_id` / `draft.recipe_id` without narrowing first.
 */
type StartingPointFields =
  /** No starting point: neither id goes on the wire. */
  | Readonly<{ template_id?: undefined; template_input?: undefined; recipe_id?: undefined }>
  /** A built-in template, and the input it declared, if any. */
  | Readonly<{
    template_id: string;
    template_input?: Readonly<Record<string, unknown>>;
    recipe_id?: undefined;
  }>
  /** A user recipe. It takes no `template_input`: that field is only accepted
   *  alongside `template_id`. */
  | Readonly<{ recipe_id: string; template_id?: undefined; template_input?: undefined }>;

export type NewTrackDraft = Readonly<{
  /**
   * What the user typed — the track's intent, and **not** its title.
   *
   * The caller creates the track with no `title` — the kernel stores the empty
   * string and the planner agent names it later (#1211). This text's destination
   * is the new track's planner agent as its first message, which the caller puts
   * on the create as `first_message` (#1299).
   *
   * **Verbatim, and never blank.** It is the reader's own string — leading and
   * trailing whitespace included, because the kernel delivers what it is given
   * — and the composer refuses to submit one the kernel would read as blank
   * (`isBlankForKernel`), which would be a 400 nobody asked for.
   */
  message: string;
  /**
   * Absolute path, **or the key is absent**. Absent is not "the empty string":
   * the caller distinguishes the two to decide whether the request carries
   * `cwd` / `attach_folder` at all, and an empty string is a legal-looking
   * value that would take the attached branch with a path that cannot work.
   */
  cwd?: string;
}> & StartingPointFields;

export type NewTrackFormProps = Readonly<{
  submitting: boolean;
  error: string | null;
  /**
   * Templates the user may start from, from `GET /api/track-templates`. An
   * empty canonical roster is fully usable when the Area has no saved
   * template. If an Area preference is still unresolved, Create stays blocked
   * until the roster resolves or the reader explicitly chooses No template.
   */
  templates: readonly TrackTemplate[];
  /** Distinguishes an empty canonical roster from a read still in flight. */
  templatesLoaded: boolean;
  /**
   * Set when the template read failed. It is a visible roster notice rather
   * than the form's create-error channel. The composer may still submit with
   * No template, but an unresolved saved Area preference fails closed until
   * the reader explicitly clears it.
   */
  templatesError?: string | null;
  /** Snapshot of the Area preferences when this route opened. */
  initialTemplateId: string | null;
  initialCwd: string | null;
  /**
   * The reader's own recipes, from `GET /api/track-recipes` (#1292). Empty is
   * the ordinary day-one state and is not an error: the menu then looks
   * exactly as it did before recipes existed.
   *
   * Defaulted rather than required so that a caller which has no recipe read
   * — there is one such surface in the tests, and there may be others later —
   * gets the built-ins-only picker instead of a type error, which is the same
   * degradation `templates: []` already has.
   */
  recipes?: readonly TrackRecipe[];
  /**
   * Open the manage-recipes screen. Injected because `features/**` may not
   * import `app/**`, so the navigation is the router's to perform.
   */
  onManageRecipes: () => void;
  /**
   * The folder picker's read port. Injected: `ui/` primitives never reach a
   * transport, and `features/**` may not import `app/**` — so the port is
   * created at the composition layer (`app/providers/directory.ts`) and passed
   * down. Required, not optional: a call site that forgot it would render a
   * picker that silently lists nothing.
   */
  listDirectory: ListDirectory;
  onSubmit: (draft: NewTrackDraft) => void;
}>;

/** The one template whose inputs this form knows how to collect. */
const ISSUE_DEVELOPMENT = 'issue-development';

/**
 * The greeting, by the reader's own clock.
 *
 * Taken as an argument rather than read inside, so the boundaries are testable
 * without freezing time globally. The cuts are the ordinary ones — morning
 * until noon, afternoon until 18:00, evening after — and the local hour is the
 * right clock precisely because this string is small talk: it is correct when
 * it matches the light outside the reader's window, and no server time zone
 * knows that.
 */
export function greetingFor(now: Date): string {
  const hour = now.getHours();
  if (hour < 12) return 'Good morning';
  if (hour < 18) return 'Good afternoon';
  return 'Good evening';
}

/**
 * The composer field's accessible name, and the placeholder that says the same
 * thing to everyone else.
 *
 * Not rendered as a label: the composer is the surface, and a label above it
 * would spend a row saying what the placeholder already says. Hidden, not
 * absent — an unnamed textbox is unusable by screen reader and by voice control
 * alike.
 */
const TASK_LABEL = 'What this track should do';

const TASK_PLACEHOLDER = 'What should this track do?';

/** The way back to the managed default, which exists nowhere else. */
const FOLDER_CLEAR_LABEL = 'Use a Neige workspace instead';

/** Mirrors the enum in the bound plugin's `input_schema`. */
type MergePolicy = 'hold-for-ratify' | 'auto-merge';

/**
 * A template takes input iff a running trusted plugin is bound to it, which is
 * exactly when the read returned an `input_schema`. Branching on that instead
 * of on the id keeps this in step with what the create path will accept: with
 * the plugin stopped, `issue-development` still seeds its report and must be
 * offered — just without the fields the kernel would then reject.
 */
function needsInput(template: TrackTemplate | undefined): boolean {
  return template?.input_schema != null;
}

export function NewTrackForm({
  submitting, error, templates, templatesLoaded, templatesError = null,
  initialTemplateId, initialCwd, recipes = [], onManageRecipes, listDirectory, onSubmit,
}: NewTrackFormProps) {
  const fieldId = useId();
  // Creation preferences are a route-opening snapshot. Area events may update
  // this prop while the same New Track route remains mounted; mixing that live
  // value with the existing local selection would silently clear an unresolved
  // opening default instead of either keeping or adopting one coherent state.
  const openingTemplateId = useRef(initialTemplateId).current;
  const [message, setMessage] = useState('');
  const [selected, setSelected] = useState<StartingPoint>(openingTemplateId === null
    ? NO_STARTING_POINT
    : { kind: 'template', id: openingTemplateId });
  const [issueUrl, setIssueUrl] = useState('');
  const [autoMerge, setAutoMerge] = useState(false);
  const [cwd, setCwd] = useState(initialCwd ?? '');
  const [browsing, setBrowsing] = useState(false);
  const composerHostRef = useRef<HTMLDivElement | null>(null);
  const folderId = `${fieldId}-folder`;
  const triggerId = `${fieldId}-start-from-trigger`;

  /*
   * The caret starts in the field (#1161's rule, on a route instead of a
   * dialog): this page exists to be typed into, and arriving with focus on the
   * document means the first thing the reader types goes nowhere.
   *
   * Found by query rather than by ref because the element that takes focus is
   * astryx's `contenteditable`, and `ChatComposerInput` forwards its DOM `ref`
   * to the *wrapper* around it (`ChatComposerInput.tsx` — `ref` at the outer
   * element, `editableRef` at the editable). A wrapper is not focusable, so a
   * ref would silently focus nothing.
   *
   * Mount-only, and that is the point: a later render must not yank the caret
   * back from a chip the reader has just opened.
   */
  useEffect(() => {
    const field = composerHostRef.current?.querySelector<HTMLElement>('[contenteditable="true"]');
    field?.focus();
  }, []);

  /*
   * A starting point that vanished between renders (the list refetched without
   * it, or the reader deleted the recipe in the manage screen) must not leave a
   * selection pointing at nothing; falling back to no template is the safe
   * direction — it always submits. A persisted Area default is the exception:
   * silently clearing that preference would create the Track from a different
   * starting point, so it stays selected and blocks Create until the roster
   * resolves or the reader explicitly chooses another row.
   *
   * **Each lookup is confined to its own id space by the tag.** Before #1292
   * this was one `templates.find` against a bare string, which for a recipe id
   * asked the wrong list: a deleted recipe whose id equalled a template key
   * would have resolved to that template and created a track from something
   * the reader never chose, silently. The two `find`s below can only ever
   * answer about the kind that was selected.
   */
  const chosen = selected.kind === 'template'
    ? templates.find((template) => template.id === selected.id)
    : undefined;
  const chosenRecipe = selected.kind === 'recipe'
    ? recipes.find((recipe) => recipe.id === selected.id)
    : undefined;
  const unresolvedAreaDefault = openingTemplateId !== null
    && selected.kind === 'template'
    && selected.id === openingTemplateId
    && chosen === undefined;
  const effectiveSelection: StartingPoint = selected.kind === 'none'
    || (selected.kind === 'template' && chosen !== undefined)
    || (selected.kind === 'recipe' && chosenRecipe !== undefined)
    || unresolvedAreaDefault
    ? selected
    : NO_STARTING_POINT;
  const wantsInput = needsInput(chosen);
  const issueDev = wantsInput
    && effectiveSelection.kind === 'template'
    && effectiveSelection.id === ISSUE_DEVELOPMENT;
  const templatePending = unresolvedAreaDefault && !templatesLoaded && templatesError === null;
  const parsedIssue = issueDev ? parseGitHubIssueUrl(issueUrl) : null;

  // Fail-closed: a bound template this build has no editor for cannot be
  // submitted, because the kernel requires the input its schema declares and
  // guessing at it would trade a readable block for a 400.
  const unsupportedInput = wantsInput && !issueDev;
  const issueUrlTouched = issueUrl.trim() !== '';
  const issueUrlBad = issueDev && issueUrlTouched && parsedIssue === null;
  const inputBlocker = unresolvedAreaDefault || unsupportedInput || (issueDev && parsedIssue === null);
  /* Blank by the *kernel's* rule, not JS's: `isBlankForKernel` is the one
     place that criterion is written (`core/domain/track.ts`), and `submit`
     below asks it the same question. A gate that used `trim()` here would
     light up Create for a draft the server refuses — see that function for the
     code point the two disagree about. */
  const valid = !isBlankForKernel(message) && !inputBlocker;
  /*
   * One status slot on the composer, and the two things that can fill it never
   * coexist: `templatesError` means the list is empty, and an empty list has no
   * bound template to be unsupported. Error vs warning is the difference that
   * matters to a reader — one blocks the submit, the other does not.
   */
  const status = templatePending
    ? { type: 'warning' as const, message: 'Loading the Area’s default template…' }
    : unresolvedAreaDefault
      ? {
        type: 'error' as const,
        message: templatesError === null
          ? 'The Area’s default template is not available in this build. Choose another starting point.'
          : `${templatesError} Choose “No template” to continue without the saved default.`,
      }
      : unsupportedInput
    ? { type: 'error' as const, message: 'This template needs input this version cannot collect yet.' }
    : templatesError !== null
      ? { type: 'warning' as const, message: `${templatesError} You can still create a track without one.` }
      : undefined;

  function submit(text: string): void {
    /* Blank refuses the submit; it does not *rewrite* it. `text` goes on to
       the caller exactly as typed — the draft is what the reader said, and the
       kernel forwards it to the agent untrimmed. An earlier cut passed
       `text.trim()` on, and `"  keep indentation  "` reached the agent with
       the indentation gone. */
    if (isBlankForKernel(text) || inputBlocker || submitting) return;
    /* Spread, not `cwd: cwd || undefined`: the caller keys the whole
       managed-vs-attached decision on whether the key is *there*, and
       `cwd: undefined` is a different object from no `cwd` for anything that
       inspects the draft before it is serialized — including the tests. A
       non-empty path travels byte-for-byte: leading/trailing spaces are legal
       POSIX path characters, not form whitespace. */
    const base = { message: text, ...(cwd === '' ? {} : { cwd }) };
    if (effectiveSelection.kind === 'none') { onSubmit(base); return; }
    /* A recipe carries `recipe_id` and stops here. It can never also carry
       `template_id`: the union has one arm at a time, so the exclusivity the
       kernel enforces with a 400 is a property of this function's shape rather
       than a rule it remembers to follow. A recipe never takes
       `template_input` either — that field is only accepted alongside
       `template_id`. */
    if (effectiveSelection.kind === 'recipe') {
      onSubmit({ ...base, recipe_id: effectiveSelection.id });
      return;
    }
    if (parsedIssue === null) { onSubmit({ ...base, template_id: effectiveSelection.id }); return; }
    // The kernel applies no schema defaults, so `merge_policy` always travels
    // explicitly. Unchecked is `hold-for-ratify`: the default direction is
    // "wait for a human", and flipping it would auto-merge by omission.
    const mergePolicy: MergePolicy = autoMerge ? 'auto-merge' : 'hold-for-ratify';
    onSubmit({
      ...base,
      template_id: effectiveSelection.id,
      template_input: { ...parsedIssue, merge_policy: mergePolicy },
    });
  }

  return (
    <div className={styles.page}>
      <VStack gap={2} className={styles.form}>
        {error !== null && (
          <Banner status="error" title={error} data-nc-new-track-error />
        )}

        {/* The mark, the greeting, and where you are — see `.masthead`. */}
        <div className={styles.masthead}>
          {/* Decorative: the greeting under it already names the page, and a
              mark that repeats it would be a second announcement of the same
              thing. The asset carries its own `<title>`, which is why it is a
              CSS mask here rather than an inlined `<svg>`. */}
          <span className={styles.mark} role="presentation" />
          <h1 className={styles.greeting}>{greetingFor(new Date())}</h1>
        </div>

        <div
          ref={composerHostRef}
          className={styles.composer}
          data-nc-new-track-message
          /*
           * Enter is **ours**, and it has to be.
           *
           * astryx's `ChatComposer.handleSubmit` is `onSubmit(trimmed);
           * updateValue('')` — it clears the controlled value unconditionally
           * and synchronously *after* calling us, while our `submit` returns
           * early whenever the draft is not submittable. Left to astryx, the
           * refusal path was: the reader's sentence disappears, nothing is
           * created, and nothing is said. Reproduced against a bound template
           * with no issue URL, where the send button is visibly disabled and
           * Enter therefore looks safe to press.
           *
           * Capturing here means astryx's handler never runs from the keyboard
           * (and the send button is our own `sendButton` override, so its
           * internal path is unused either) — so nothing clears the field
           * behind our back. Nothing needs to: a successful submit navigates
           * away and unmounts this component.
           *
           * It is also what keeps the sentence verbatim (#1299): the `trimmed`
           * in astryx's handler is astryx's own, and both of this page's
           * submit paths pass `message` — the field's value — instead.
           *
           * **Only when the field itself is the target**, and `matches` rather
           * than `closest` for a reason that is not pedantry. This handler sits
           * on the wrapper, which contains the footer chips and — because
           * astryx's popover does not portal — the open template menu; the
           * first cut omitted the target check entirely and swallowed Enter for
           * all of them, so arrowing to a template and pressing Enter created a
           * track with *no* template and navigated away from it.
           *
           * `closest` fixed that and left a subtler one: there are focusable
           * controls *inside* the editable. `ChatComposerInput` turns any paste
           * over 200 characters into a token (`useChatPasteAsToken`, on by
           * default — this call site does not pass `pasteAsToken={false}`), and
           * that token's hover card carries an `Expand` button which is a DOM
           * descendant of the `contenteditable`. Under `closest`, tabbing to
           * Expand and pressing Enter created a track instead of expanding the
           * token — and pasting a long instruction into this field is an
           * entirely ordinary thing to do.
           *
           * The keydown target while typing in a `contenteditable` *is* the
           * editable element (text nodes are not event targets), so comparing
           * the target to the field loses nothing and separates every
           * descendant control out. Enter means "activate this control"
           * everywhere except in a text field.
           *
           * The IME guard is the second reason and predates the first: Enter
           * while composing is *accepting a candidate*, not sending, so it must
           * not create a track mid-word. Same guard, same reason, as the chat
           * thread's composer.
           */
          onKeyDownCapture={(event) => {
            if (event.key !== 'Enter' || event.shiftKey) return;
            const target = event.target as HTMLElement | null;
            const field = target?.closest?.('[contenteditable="true"]') ?? null;
            if (field === null) return;
            if (target !== field) {
              /* A control *inside* the editable — a paste token's `Expand`
                 button and anything astryx adds later. Enter belongs to it, so
                 this neither submits nor `preventDefault`s (the button still
                 activates natively). It does stop propagation, because letting
                 the event reach the editable hands it to astryx's own Enter
                 handling, which submits — returning early here was the first
                 fix and it left exactly that path open. */
              event.stopPropagation();
              return;
            }
            event.stopPropagation();
            if (event.nativeEvent.isComposing) return;
            event.preventDefault();
            submit(message);
          }}
        >
          <ChatComposer
            density="spacious"
            value={message}
            onChange={setMessage}
            placeholder={TASK_PLACEHOLDER}
            isDisabled={submitting}
            onSubmit={submit}
            status={status}
            input={<ChatComposerInput label={TASK_LABEL} placeholder={TASK_PLACEHOLDER} />}
            /* ── The two settings, as chips under the sentence ─────────────────
               What this track starts from and where it runs are the same *kind* of
               thing: one optional choice each, both defaulted, both changing only
               what the sentence above them is carried out on. In the footer and
               not above the field because that is where a composer's controls
               belong — the input is the surface, and these sit under it. Each
               chip says what it is for and then what it holds, so the row needs
               no labels above it and no paragraph under it. */
            footerActions={(
              <HStack gap={1} align="center" className={styles.controls}>
                <StartingPointPill
                  templates={templates}
                  templatesLoaded={templatesLoaded}
                  recipes={recipes}
                  value={effectiveSelection}
                  onChange={setSelected}
                  onManageRecipes={onManageRecipes}
                  placement="above"
                  triggerId={triggerId}
                />

                {/* Shared with the Area editor so the two folder preferences
                    keep one compact, borderless pill contract. */}
                <FolderPill
                  buttonId={folderId}
                  value={cwd}
                  clearLabel={FOLDER_CLEAR_LABEL}
                  onBrowse={() => setBrowsing(true)}
                  onClear={() => setCwd('')}
                />
              </HStack>
            )}
          /* Astryx's own `ChatSendButton` is named "Send", which is true of
               every other composer in the app and false of this one: pressing it
               creates a track. The name is the only thing overridden — the shape,
               the icon and the position stay the composer's. */
            sendButton={(
              <Button
                type="button"
                variant="primary"
                isIconOnly
                icon={<Icon icon="arrowUp" size="sm" />}
                label={submitting ? 'Creating…' : 'Create track'}
                isDisabled={submitting || !valid}
                onClick={() => submit(message)}
              />
            )}
          />
        </div>

        {issueDev && (
          /* Under the chip that chose it, named by the template it belongs to.
             The group needs a *name*, not a second visible heading: the trigger
             already reads that title, and repeating it would be the same word
             twice in two rows. */
          <div className={styles.panel} role="group" aria-label={chosen?.title ?? ''}>
            <TextInput
              label="Issue URL"
              value={issueUrl}
              width="100%"
              placeholder="https://github.com/owner/repo/issues/123"
              /* An unfinished field is not an error: until something has been
                 typed the guidance is a description, and only a value that
                 cannot be parsed turns into `status` (which is what sets
                 `aria-invalid` and the alert). */
              description={issueUrlBad ? undefined : parsedIssue === null
                ? 'Paste the GitHub issue this track works on.'
                : `Issue #${parsedIssue.issue_number} in ${parsedIssue.repo}.`}
              status={issueUrlBad
                ? {
                  type: 'error',
                  message: 'Not a GitHub issue URL — expected https://github.com/owner/repo/issues/123.',
                }
                : undefined}
              onChange={(value) => setIssueUrl(value)}
            />
            <CheckboxInput
              label="Merge automatically once the gates converge"
              description="Off: the track waits for you to approve the merge."
              value={autoMerge}
              onChange={(checked) => setAutoMerge(checked)}
            />
          </div>
        )}
      </VStack>

      {/* The picker, as a real modal — see the header. `Dialog` renders `null`
          while closed, so the unopened case costs nothing, and it owns the
          focus trap, the Escape handling and the click-outside that a browser
          unrolled into the page had none of. */}
      <Dialog
        open={browsing}
        onClose={() => setBrowsing(false)}
        title="Choose a directory"
        wide
      >
        <DirectoryBrowser
          listDirectory={listDirectory}
          initialPath={cwd === '' ? null : cwd}
          mode="directory"
          onCancel={() => setBrowsing(false)}
          onSelect={(path) => { setCwd(path); setBrowsing(false); }}
        />
      </Dialog>
    </div>
  );
}
