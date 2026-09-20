// The CARDS module's `+`: which kind, and then what that kind needs to know. Neither piece calls an API.
// `DirectoryField` pushes its browser into the surrounding dialog, so this form is only ever hosted inside `ui/dialog`.

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

/** The `+` in the module head, and the kinds behind it. An empty registry still renders the `+` with one disabled row: a build with no creatable kind is a defect, not a design choice. */
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
  /** The picker's read port, injected. */
  listDirectory: ListDirectory;
  /** The dialog's opening focus target, bound to the first field: without one the dialog focuses its own Close button and the first keystroke closes it. */
  firstFieldRef: RefObject<HTMLInputElement | null>;
  onCancel: () => void;
  onSubmit: (values: NewCardValues) => void;
}>;

/** The declared fields of one kind. Empty means absent: an untouched `Working directory` sends no `cwd` at all rather than `""`, which the kernel would read as a path. */
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
              /* Only the first field takes the ref: a ref handed to every input would leave the last one holding it. */
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
        /* A `<label htmlFor>` pointing at a button replaces that button's contents as its accessible name, which is what is wanted. */
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
