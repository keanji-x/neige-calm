// The new-track page: one thing to say, and two optional chips under it saying
// what it is carried out on.
//
// Presentational + local form state — it never calls an API. The caller owns
// `POST /api/tracks`, `submitting`, `error`, and the template list itself. It
// does **not** yet own a first message: see "Where the sentence goes" below.
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
// all and the spec agent names the track itself through `calm.track.rename` — so
// what is left to collect here is the *intent*, and intent is a sentence, not
// a label.
//
// That changes the shape of the surface rather than just its wording. A form
// asks you to fill in fields; you have to know what each one wants before you
// can start. A composer asks you to say something, which is the only thing
// this product ever asks anywhere else — the track page's spec drawer is a
// composer, the area conversation is a composer. Creating a track was the one
// place with a different grammar, and there was no reason for it.
//
// So: `ChatComposer` from astryx, the same component the chat thread uses, with
// the two settings as footer chips. There is no Cancel button — the way out of
// a page is Back, and inventing a second one here would be a button that means
// "Back" but does not update history. And there is no separate "Create track"
// step: the send button *is* the create, and Enter reaches it.
//
// ## Where the sentence goes — and where it does not, yet (#1299)
//
// **Not** into `title`: the draft carries it as `message`, and the caller
// creates the track with no title at all.
//
// Its destination is the new track's spec card, as the first message. That
// delivery is **not implemented yet** — two review rounds showed the three-write
// sequence it needs cannot be made sound from a component (see `NewTrackRoute`),
// and #1299 moves it into the create request where the kernel can do it
// atomically. Until then the track is created and the reader says it again in
// the spec conversation, which the route opens for them on arrival.
//
// The form says so, on screen, above the send button. A field whose contents
// are quietly dropped is worse than no field; a field that tells you what it
// will and will not do is a smaller product than intended but an honest one.
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
// ## The folder is optional and empty by default (#1147 S3)
//
// Left empty, the draft carries no `cwd` at all and the caller's POST omits
// `cwd` / `attach_folder`, which is the kernel's *managed*-workspace branch: it
// allocates a directory under the workspace root, `git init`s it, and owns it.
// Filled in, the track is *attached* to a repository the user already has, which
// the kernel never creates, moves or deletes. Create time is the only entry
// into that choice — `managed → attached` after the fact exists as an API and
// has no UI — so an always-visible optional control is the whole feature, not a
// shortcut for one.
//
// ## The template (#1209)
//
// "No template" is a first-class option and the default, and it is **not** a
// row the server sent: it is the absence of a template, i.e. a create with no
// `template_id`. Everything about this list is arranged so that staying on it
// is free. In particular `templates` may be empty because the read failed or
// has not landed, and the composer is fully usable in that state: this is the
// app's only track-creation entry point, and a failed list read must not be able
// to close it.
//
// The words on screen are the reader's, not the codebase's. Both chips name
// their **default** rather than asking a question — "No template" and "Neige
// workspace" — because neither has an undecided state: leave them alone and
// those are the track you get. See `NO_TEMPLATE` and `FOLDER_PLACEHOLDER` for
// the long form.
//
// One concept, one word, one field: the list, the chip and the wire all say
// *template* / `template_id`. (#1209 removed the vocabulary seam this comment
// used to describe, where the read side and the write side used different
// words for the same thing.)
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
// an `aria-label`, because the chip's *text* is the bare choice and a name
// read on its own has to say which kind of choice it is — and the chosen item
// carries a check icon plus a `VisuallyHidden` "Selected".
//
// ### Two astryx limits this shape runs into, measured and left standing
//
// Neither is fixable from here — both live inside `@astryxdesign/core` — so
// they are written down rather than worked around with a local fork.
//
//  1. **The menu's accessible name is the current selection, not "Template".**
//     `DropdownMenu` names its popup from its trigger's *label*
//     (`aria-label={button.label}`, `DropdownMenu.tsx`) and not from the
//     trigger's computed name, so the `aria-label` that makes the trigger read
//     "Template: Small change" does not reach the popup: a reader who opens it
//     on a chosen template hears "Small change menu". Unset the two coincide.
//  2. **The hover card's `role="dialog"` is a DOM descendant of the
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
import { DropdownMenu, DropdownMenuItem } from '@astryxdesign/core/DropdownMenu';
import { HoverCard } from '@astryxdesign/core/HoverCard';
import { HStack } from '@astryxdesign/core/HStack';
import { Icon } from '@astryxdesign/core/Icon';
/* The app's own icon set, for the one glyph astryx does not ship: `folder`.
   Aliased because both are called `Icon` and both are used here — astryx's for
   the controls it also owns (check, close, the send arrow), this one for the
   folder chip, which is the same glyph `DirectoryField` draws so the product's
   two folder controls do not show different folders. */
import { Icon as AppIcon } from '../../../ui/icon/public.tsx';
import { List, ListItem } from '@astryxdesign/core/List';
import { TextInput } from '@astryxdesign/core/TextInput';
import { VisuallyHidden } from '@astryxdesign/core/VisuallyHidden';
import { VStack } from '@astryxdesign/core/VStack';

import { parseGitHubIssueUrl } from '../../../../../core/domain/issue-url.ts';
import type { TrackTemplate } from '../../../../../core/domain/track.ts';
import type { ListDirectory } from '../../../ui/directory-browser/public.tsx';
import { DirectoryBrowser } from '../../../ui/directory-browser/public.tsx';
import { Dialog } from '../../../ui/dialog/public.tsx';
import { useState } from '../../../ui/state/public.ts';
import styles from './new-track.module.css';

export type NewTrackDraft = Readonly<{
  /**
   * What the user typed — the track's intent, and **not** its title.
   *
   * The caller creates the track with no `title` — the kernel stores the empty
   * string and the spec agent names it later (#1211). This text's destination
   * is the new track's spec card as its first message, but **that delivery is
   * not implemented yet** (#1299); today the caller creates the track and the
   * reader says it again in the conversation the track page opens. Always
   * non-empty and already trimmed: the composer will not submit otherwise.
   */
  message: string;
  /** Absent for no template — never `null` or `''`, which the kernel 400s. */
  template_id?: string;
  template_input?: Readonly<Record<string, unknown>>;
  /**
   * Absolute path, **or the key is absent**. Absent is not "the empty string":
   * the caller distinguishes the two to decide whether the request carries
   * `cwd` / `attach_folder` at all, and an empty string is a legal-looking
   * value that would take the attached branch with a path that cannot work.
   */
  cwd?: string;
}>;

export type NewTrackFormProps = Readonly<{
  submitting: boolean;
  error: string | null;
  /**
   * Templates the user may start from, from `GET /api/track-templates`. An
   * empty array is a legitimate, fully working state — no-template only.
   */
  templates: readonly TrackTemplate[];
  /**
   * Set when the template read failed. It is a *notice*, not an error: the
   * composer still submits. Told rather than hidden, so "where did my templates
   * go" has an answer on screen.
   */
  templatesError?: string | null;
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
 * Selection sentinel for "no template".
 *
 * `''` because it is the *absence* of a template id, which no server row can
 * ever collide with, and because that absence is what goes on the wire.
 */
const BLANK = '';

/**
 * What the template chip says when nothing has been picked.
 *
 * It names the **default**, not a question. An earlier cut had it ask ("Choose
 * a template") on the theory that an unset control should say what it is for.
 * That is right for a control whose unset state is *undecided*, and wrong for
 * this one: there is no undecided state here — no template is a real, working
 * choice, it is the one this page opens on, and it is what gets created if the
 * chip is never touched. A control that asks a question it has already answered
 * makes a settled default look like an outstanding task.
 *
 * `No template` and not `Blank`: `Blank` was the codebase's word for "no
 * `template_id` on the wire", and it had leaked onto a chip a person reads
 * before they know this app has templates at all. The same string serves as the
 * chip and as the menu's first row, which is now a plain identity rather than
 * two strings that have to be kept in step.
 */
const NO_TEMPLATE = 'No template';

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

/**
 * What the composer says about where the sentence goes, until #1299 lands.
 *
 * It states the cost plainly, because the cost is real: this slice does not
 * carry the sentence, so the reader will retype it in the conversation the track
 * page opens for them. Saying so *before* they type is the difference between a
 * known limitation and a field that silently eats input — and it is why this
 * string exists rather than the page quietly discarding the text. It goes when
 * #1299 lands, along with the retyping.
 */
const PENDING_DELIVERY_NOTICE = "You'll say this again in the track's chat";
const TASK_PLACEHOLDER = 'What should this track do?';

/**
 * What the folder chip says while no folder has been picked — its visible text,
 * its accessible name and its hover string at once.
 *
 * Like the template chip, it names the default rather than asking: leaving this
 * alone is not an omission, it is the *managed* branch — the kernel allocates a
 * directory under the workspace root, `git init`s it and owns it — and that is
 * the right answer for most tracks. Picking a folder is the exception, so the
 * chip states what will happen and stays out of the way.
 *
 * It is the chip's *text* only. The accessible name is built separately from
 * `FOLDER_PURPOSE`, because a name read on its own has to say which kind of
 * control it is — and once a folder is picked, "Neige workspace: /srv/app"
 * would be the default's name glued to the value that replaced it.
 */
const FOLDER_PLACEHOLDER = 'Neige workspace';
/**
 * What the folder control *is*, for the accessible name — which the chip's own
 * text cannot carry in either state: unset it is the default's name, and set it
 * is a bare basename ("app"), which tells a reader nothing about which `app`.
 * The full path rides in the name too, for the same reason.
 */
const FOLDER_PURPOSE = 'Folder';
/** The way back to the managed default, which exists nowhere else. */
const FOLDER_CLEAR_LABEL = 'Use a Neige workspace instead';

/**
 * The segment that identifies a path: its last one, or `/` for the root, which
 * has none. A chip sits in a row of chips and cannot hold
 * `/home/kenji/src/neige-calm` without becoming the row; the whole path is one
 * hover or one screen reader away, in the name.
 */
function basenameOf(path: string): string {
  const trimmed = path.replace(/\/+$/, '');
  return trimmed === '' ? '/' : trimmed.slice(trimmed.lastIndexOf('/') + 1);
}

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
  submitting, error, templates, templatesError = null, listDirectory, onSubmit,
}: NewTrackFormProps) {
  const fieldId = useId();
  const [message, setMessage] = useState('');
  const [selected, setSelected] = useState<string>(BLANK);
  const [issueUrl, setIssueUrl] = useState('');
  const [autoMerge, setAutoMerge] = useState(false);
  const [cwd, setCwd] = useState('');
  const [browsing, setBrowsing] = useState(false);
  const composerHostRef = useRef<HTMLDivElement | null>(null);
  const folderId = `${fieldId}-folder`;
  const noticeId = `${fieldId}-pending-delivery`;
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
    /*
     * The notice is the field's accessible *description*, not a second label:
     * it states what will happen to what you type, and a reader who cannot see
     * it needs it more than one who can — this page puts the caret straight
     * into the field, so there is no moment on the way in where unassociated
     * text nearby would be read out.
     *
     * Set on the element imperatively because `ChatComposerInput` spreads its
     * rest props onto the *wrapper* around the editable, not the editable
     * itself (`ChatComposerInput.tsx` — `ref`/`mergeProps` at the outer node,
     * a separate prop set on the `contenteditable`), so `aria-describedby`
     * passed as a prop lands on a node with no role and describes nothing.
     * Measured: the assertion in `public.test.tsx` read `null` off the field
     * until this moved here.
     */
    field?.setAttribute('aria-describedby', noticeId);
    field?.focus();
  }, [noticeId]);

  // A template that vanished between renders (the list refetched without it)
  // must not leave a selection pointing at nothing; falling back to no template
  // is the safe direction — it always submits.
  const chosen = templates.find((template) => template.id === selected);
  const effectiveSelection = selected === BLANK || chosen ? selected : BLANK;
  const wantsInput = needsInput(chosen);
  const issueDev = wantsInput && effectiveSelection === ISSUE_DEVELOPMENT;
  const parsedIssue = issueDev ? parseGitHubIssueUrl(issueUrl) : null;

  // Fail-closed: a bound template this build has no editor for cannot be
  // submitted, because the kernel requires the input its schema declares and
  // guessing at it would trade a readable block for a 400.
  const unsupportedInput = wantsInput && !issueDev;
  const issueUrlTouched = issueUrl.trim() !== '';
  const issueUrlBad = issueDev && issueUrlTouched && parsedIssue === null;
  const inputBlocker = unsupportedInput || (issueDev && parsedIssue === null);
  const valid = message.trim() !== '' && !inputBlocker;
  /* The folder control's accessible name *and* its hover string — one value,
     because they answer the same question and must not drift apart. Neither is
     the chip's text: unset that text is the default's name, and set it is a
     bare basename, and neither survives being read on its own. */
  const folderName = cwd === '' ? `${FOLDER_PURPOSE}: ${FOLDER_PLACEHOLDER}` : `${FOLDER_PURPOSE}: ${cwd}`;

  /*
   * One status slot on the composer, and the two things that can fill it never
   * coexist: `templatesError` means the list is empty, and an empty list has no
   * bound template to be unsupported. Error vs warning is the difference that
   * matters to a reader — one blocks the submit, the other does not.
   */
  const status = unsupportedInput
    ? { type: 'error' as const, message: 'This template needs input this version cannot collect yet.' }
    : templatesError !== null
      ? { type: 'warning' as const, message: `${templatesError} You can still create a track without one.` }
      : undefined;

  function submit(text: string): void {
    const trimmed = text.trim();
    if (trimmed === '' || inputBlocker || submitting) return;
    /* Spread, not `cwd: folder || undefined`: the caller keys the whole
       managed-vs-attached decision on whether the key is *there*, and
       `cwd: undefined` is a different object from no `cwd` for anything that
       inspects the draft before it is serialized — including the tests. */
    const folder = cwd.trim();
    const base = { message: trimmed, ...(folder === '' ? {} : { cwd: folder }) };
    if (effectiveSelection === BLANK) { onSubmit(base); return; }
    if (parsedIssue === null) { onSubmit({ ...base, template_id: effectiveSelection }); return; }
    // The kernel applies no schema defaults, so `merge_policy` always travels
    // explicitly. Unchecked is `hold-for-ratify`: the default direction is
    // "wait for a human", and flipping it would auto-merge by omission.
    const mergePolicy: MergePolicy = autoMerge ? 'auto-merge' : 'hold-for-ratify';
    onSubmit({
      ...base,
      template_id: effectiveSelection,
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
                <DropdownMenu
                  placement="above"
                  button={{
                    id: triggerId,
                    label: chosen?.title ?? NO_TEMPLATE,
                    /* The chip's text is the choice; its *name* has to survive
                       being read on its own, out of the row, with nothing beside
                       it — so it says which kind of choice it is. Unset the two
                       coincide, because "Choose a template" already is that
                       sentence. */
                    'aria-label': `Template: ${chosen?.title ?? NO_TEMPLATE}`,
                    variant: 'secondary',
                    size: 'sm',
                    className: styles.trigger,
                  }}
                >
                  {/* The absence of a template is an alternative like any other,
                      and it is the one the composer opens on. It carries no hover
                      card because it has no tasks to show — its whole content is
                      its name. */}
                  <TemplateChoice
                    label={NO_TEMPLATE}
                    isSelected={effectiveSelection === BLANK}
                    onSelect={() => setSelected(BLANK)}
                  />
                  {templates.map((template) => (
                    <TemplateChoice
                      key={template.id}
                      label={template.title}
                      tasks={template.tasks}
                      isSelected={effectiveSelection === template.id}
                      onSelect={() => setSelected(template.id)}
                    />
                  ))}
                </DropdownMenu>

                {/* The folder, #1147 S3. The chip's text is the default's name
                    until a folder is picked and the folder's basename after; the
                    name says which control it is and carries the full path,
                    because neither of those texts survives being read alone.
                    `aria-haspopup="dialog"` because it opens one — see the
                    `Dialog` at the end of this component. */}
                <Button
                  type="button"
                  id={folderId}
                  variant="secondary"
                  size="sm"
                  className={styles.trigger}
                  aria-haspopup="dialog"
                  aria-label={folderName}
                  icon={<AppIcon name="folder" size="sm" />}
                  label={cwd === '' ? FOLDER_PLACEHOLDER : basenameOf(cwd)}
                  onClick={() => setBrowsing(true)}
                  /* The one attribute astryx has no prop for, passed through as
                     a rest prop: it is what makes a chip truncated to a
                     basename one hover from readable. astryx drops `title` from
                     `BaseProps` in favour of a `tooltip` prop, which would mount
                     a floating layer on a control whose whole job is to open
                     one. Same reasoning, and the same shape, as
                     `DirectoryField`'s. */
                  {...{ title: folderName }}
                />

                {/* Create time is the only entry into the attached choice, so the
                    way *back* to the managed default has to exist here too —
                    there is no later screen for it. It appears beside the folder
                    it undoes and only once there is one; icon-only, because a
                    control that undoes a choice should not be wider than the
                    choice. */}
                {cwd !== '' && (
                  <Button
                    type="button"
                    variant="ghost"
                    size="sm"
                    isIconOnly
                    icon={<Icon icon="close" size="sm" />}
                    label={FOLDER_CLEAR_LABEL}
                    onClick={() => setCwd('')}
                  />
                )}
              </HStack>
            )}
            /* #1299 — said where it is read, not in a tooltip and not after the
             fact: the sentence starts the track off but is not delivered to the
             agent yet, so the reader knows before they type that they will be
             repeating it. Removed in the same change that lands delivery. */
          headerContext={<span id={noticeId} className={styles.notice}>{PENDING_DELIVERY_NOTICE}</span>}
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

/**
 * One alternative in the template menu, and — when it has tasks — the
 * "what will this give me" card behind it.
 *
 * ## The row is the trigger
 *
 * An earlier cut hung the card off a separate "N tasks" label in the row's
 * `endContent`. `HoverCard` renders a string child as a focusable
 * `<span tabIndex={0}>`, so that label was a *second* tab stop inside every
 * row — a composite control is supposed to be one stop.
 *
 * The card now hangs off the option itself. It costs no tab stop, because
 * `DropdownMenuItem` renders `tabIndex={-1}`: the menu is entered from its
 * trigger and walked with arrow keys, and `useListFocus.focusIndex` moves real
 * DOM focus onto the `[role="menuitem"]` element. `focusin` on the option is
 * therefore what opens the card — the keyboard gets the same affordance as the
 * pointer, on the same element, with no stop of its own.
 *
 * `focusTrigger="always"` is load-bearing and not defensive: the default
 * `'auto'` attaches focus listeners only when `useHoverCard`'s `isFocusable`
 * says so, and that helper returns `false` for an element with `tabindex="-1"`
 * — which is every menu item here. Left at `'auto'` the card would be
 * hover-only, i.e. the original defect with a new shape.
 *
 * ## The one sharp edge, reported as measured
 *
 * `useHoverCard` attaches a *native* `keydown` listener to its trigger that
 * calls `stopPropagation()` on Escape. The trigger here is the menu item, so
 * that listener sits below `DropdownMenu`'s React `onKeyDown` — which is
 * delegated at the root and therefore never runs. Escape's effect on the
 * *menu* is thereby handed to the engine's close request against
 * `popover="auto"`, and which layer that request lands on turns on whether the
 * DOM listener already hid the card: measured in Chromium it goes both ways
 * between runs, and in jsdom the menu never closes at all.
 *
 * The card itself always closes on Escape, and the menu always closes on Tab —
 * `DropdownMenu` handles Tab itself, per the APG menu-button pattern — so the
 * picker is not a keyboard trap.
 *
 * ## Why a HoverCard and not a Tooltip
 *
 * A tooltip is short non-interactive text and closes the moment the pointer
 * leaves the trigger, so a scrolling list inside one is unreachable — you
 * cannot move the mouse into it. `HoverCard` keeps itself open while the
 * pointer or focus is inside its content.
 *
 * ## The content
 *
 * Nothing here is authored copy: `key` and `goal` come from the `task` blocks
 * the created track's report is seeded with, so this cannot drift from what the
 * template actually does the way a hand-written description would (#1209
 * declined to add one for exactly that reason).
 */
function TemplateChoice({ label, tasks, isSelected, onSelect }: Readonly<{
  label: string;
  tasks?: TrackTemplate['tasks'];
  isSelected: boolean;
  onSelect: () => void;
}>) {
  const item = (
    <DropdownMenuItem
      label={label}
      onClick={onSelect}
      /* `DropdownMenuItem` is a `role="menuitem"` with no `aria-checked` to
         set — astryx exposes no `menuitemradio`. The check icon alone would
         say nothing to a screen reader, so the state is also spelled out in
         the item's accessible name. */
      endContent={isSelected ? (
        <>
          <Icon icon="check" size="sm" color="accent" />
          <VisuallyHidden>Selected</VisuallyHidden>
        </>
      ) : undefined}
    />
  );
  if (tasks === undefined || tasks.length === 0) return item;
  return (
    <HoverCard
      placement="end"
      focusTrigger="always"
      content={(
        // Scrolling and a ceiling are ours: `HoverCard` has no max-height, and
        // its `className`/`xstyle` props never reach the rendered layer, so the
        // only place to bound the height is the content we pass in.
        <span className={styles.taskScroll}>
          <List listStyle="decimal" density="compact">
            {tasks.map((task) => (
              <ListItem key={task.key} label={task.key} description={task.goal} />
            ))}
          </List>
        </span>
      )}
    >
      {item}
    </HoverCard>
  );
}
