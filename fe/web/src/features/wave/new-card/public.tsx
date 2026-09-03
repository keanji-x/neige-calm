// The CARDS module's `+`: which kind, and then what that kind needs to know.
//
// Two presentational pieces, and neither calls an API — `app/router` owns every
// create, the same way it owns the wave create behind `features/area/new-wave`.
//
// ## Why the menu is a projection of the registry
//
// The panel used to hold one button that made a terminal, because terminal was
// the only kind this build could draw. The list of kinds is now read off the
// card registry (`cardAddMenuEntries`), so a kind appears here **iff** this
// bundle has an entry that can render it and the entry declares an `addPanel`.
// A hand-written list here would have been the second place that decides what a
// card kind is, and the failure it invites is specific: an offered kind whose
// card the board cannot draw creates a row the reader can never open.
//
// ## Why the fields are declared, not hand-written per kind
//
// A create form per kind is how one kind gets a `Title` label and the next gets
// `Name`. The entry declares `{ key, label, kind }` and this form renders it, so
// the shapes cannot drift; what the form does *not* do is decide what the values
// mean — `app/router` maps them onto the right endpoint, because which endpoint
// a kind takes is a fact about the kernel, not about the form.
//
// `directory` and `file` both render `ui/schema-form`'s `DirectoryField`, which
// pushes its browser into the *surrounding* dialog rather than opening a second
// one. That is the reason this form is only ever hosted inside `ui/dialog`.

import { useId, type RefObject } from 'react';
import { Banner } from '@astryxdesign/core/Banner';
import { Button } from '@astryxdesign/core/Button';
import { Field } from '@astryxdesign/core/Field';
import { HStack } from '@astryxdesign/core/HStack';
import { TextInput } from '@astryxdesign/core/TextInput';
import { VStack } from '@astryxdesign/core/VStack';

import { DropdownMenu, DropdownMenuItem } from '@astryxdesign/core/DropdownMenu';

import type { CardAddMenuEntry } from '../../../systems/cards/public.js';
import type { ListDirectory } from '../../../ui/directory-browser/public.tsx';
import { Icon } from '../../../ui/icon/public.tsx';
import { DirectoryField } from '../../../ui/schema-form/fields/DirectoryField/public.tsx';
import { useState } from '../../../ui/state/public.ts';
import styles from './new-card.module.css';

export type NewCardValues = Readonly<Record<string, string>>;

export type AddCardMenuProps = Readonly<{
  entries: readonly CardAddMenuEntry[];
  /** Fired with the picked entry. The caller decides between "create now" and
   *  "collect its fields first" — this menu never creates anything. */
  onSelect: (entry: CardAddMenuEntry) => void;
}>;

/**
 * The `+` in the module head, and the kinds behind it.
 *
 * ## astryx's `DropdownMenu`, not a hand-built one
 *
 * The first cut drove `ui/menu` and dressed it in a local CSS module: the
 * surface, the radius, the item height and the hover were all restated here.
 * Two of them were wrong — a border on a surface whose own rule is "colour and
 * radius, no outline", and an item that was a `<button>` inside a full-width
 * `<li>`, so the hover highlight was the width of the *word* (50px inside a
 * 172px row, measured) rather than the row. Both are defects that only exist
 * because the styling was rewritten instead of used.
 *
 * `DropdownMenu` is the same control `features/area/new-wave`'s Start-from
 * picker uses, and it owns all of it: the popover surface and its layer, the
 * item shape and its hover/active states, the APG menu-button keyboard model
 * (arrows move real focus, Tab closes, Escape restores), and typeahead. What
 * is left here is which entries exist and what picking one means.
 *
 * The trigger is `isIconOnly` with the app's own `+` glyph, so it stays the
 * same mark as every other panel-module head, and its accessible name is the
 * one word that says what it does.
 *
 * An empty registry still renders the `+`, and the menu says so: a build that
 * registered no creatable kind is a defect, and a missing button reads as "this
 * panel has no add" rather than as the fault it is. The row is disabled because
 * there is nothing behind it to pick.
 */
export function AddCardMenu({ entries, onSelect }: AddCardMenuProps) {
  return (
    <DropdownMenu
      placement="below"
      button={{
        label: 'Add card',
        icon: <Icon name="plus" />,
        isIconOnly: true,
        variant: 'ghost',
        size: 'sm',
        className: styles.trigger,
      }}
    >
      {entries.length === 0
        ? <DropdownMenuItem label="No card kinds available" isDisabled />
        : entries.map((entry) => (
          <DropdownMenuItem
            key={entry.type}
            label={entry.label}
            onClick={() => onSelect(entry)}
          />
        ))}
    </DropdownMenu>
  );
}

export type NewCardFormProps = Readonly<{
  entry: CardAddMenuEntry;
  submitting: boolean;
  error: string | null;
  /** The picker's read port, injected — see `NewWaveFormProps.listDirectory`. */
  listDirectory: ListDirectory;
  /**
   * The dialog's opening focus target, bound to the first field. Required for
   * the reason #1161 records: without one the dialog focuses its own Close
   * button, and the first thing typed closes the dialog instead of filling it.
   *
   * A kind with no fields never renders this form at all, so there is always a
   * first field for the ref to land on.
   */
  firstFieldRef: RefObject<HTMLInputElement | null>;
  onCancel: () => void;
  onSubmit: (values: NewCardValues) => void;
}>;

/**
 * The declared fields of one kind, and nothing else.
 *
 * Values are strings throughout and empty means absent: the caller drops empty
 * keys before it builds a request, so an untouched `Working directory` sends no
 * `cwd` at all rather than `""` — which the kernel would read as a path.
 */
export function NewCardForm({
  entry, submitting, error, listDirectory, firstFieldRef, onCancel, onSubmit,
}: NewCardFormProps) {
  const fieldId = useId();
  const [values, setValues] = useState<NewCardValues>({});
  const missingRequired = entry.fields.some(
    (field) => field.required === true && (values[field.key] ?? '').trim() === '',
  );
  const valid = !missingRequired;

  return (
    <VStack
      as="form"
      gap={2}
      className={styles.form}
      onSubmit={(event) => {
        event.preventDefault();
        if (!valid || submitting) return;
        onSubmit(values);
      }}
    >
      {error !== null && <Banner status="error" title={error} data-nc-new-card-error />}

      {entry.fields.map((field, index) => {
        const id = `${fieldId}-${field.key}`;
        const value = values[field.key] ?? '';
        const set = (next: string) => setValues((current) => ({ ...current, [field.key]: next }));
        if (field.kind === 'text') {
          return (
            <TextInput
              key={field.key}
              /* Only the first field takes the ref: it is the dialog's opening
                 focus, and a ref handed to every input would leave the last one
                 holding it. */
              ref={index === 0 ? firstFieldRef : undefined}
              label={field.label}
              placeholder={field.placeholder}
              description={field.hint}
              value={value}
              width="100%"
              isRequired={field.required}
              onChange={set}
            />
          );
        }
        /* A `<label htmlFor>` pointing at a button replaces that button's
           contents as its accessible name, which is what is wanted: the name is
           the field's label, and the path it holds is its value. */
        return (
          <Field key={field.key} label={field.label} inputID={id} description={field.hint}>
            <DirectoryField
              id={id}
              value={value}
              onChange={set}
              listDirectory={listDirectory}
              mode={field.kind === 'file' ? 'file' : 'directory'}
              placeholder={field.placeholder}
            />
          </Field>
        );
      })}

      <HStack gap={1} justify="end">
        <Button type="button" label="Cancel" variant="ghost" onClick={onCancel} />
        <Button
          type="submit"
          variant="primary"
          label={submitting ? 'Creating…' : `Create ${entry.label}`}
          isDisabled={submitting || !valid}
        />
      </HStack>
    </VStack>
  );
}
