// One editor for both Area creation and Area settings; a pure form, the shell owns
// the Dialog, mutations and directory transport.

import { Banner } from '@astryxdesign/core/Banner';
import { Button } from '@astryxdesign/core/Button';
import { HStack } from '@astryxdesign/core/HStack';
import { TextInput } from '@astryxdesign/core/TextInput';
import { VStack } from '@astryxdesign/core/VStack';
import { useEffect, useId, type FormEvent, type RefObject } from 'react';

import { DirectoryBrowser, type ListDirectory } from '../../../ui/directory-browser/public.tsx';
import { useDialogView } from '../../../ui/dialog/public.tsx';
import { useState } from '../../../ui/state/public.ts';
import { FolderPill } from '../default-pills/public.tsx';
import styles from './area-editor.module.css';

// Astryx's public type does not expose the native `required` constraint, and
// `isRequired` would duplicate this dialog's only field label with "· Required".
const NATIVE_REQUIRED_INPUT_PROPS = Object.freeze({ required: true });

export type AreaEditorValues = Readonly<{
  name: string;
  defaultCwd: string | null;
}>;

export type AreaEditorPatch = Readonly<{
  name?: string;
  defaultCwd?: string | null;
}>;

export type AreaEditorFormProps = Readonly<{
  submitting: boolean;
  /** An unconfirmed creation must retry its original payload. */
  locked: boolean;
  cancelLabel: string;
  error: string | null;
  listDirectory: ListDirectory;
  nameInputRef: RefObject<HTMLInputElement | null>;
  submitLabel: string;
  onCancel: () => void;
  onSubmit: (values: AreaEditorValues) => void;
  initial: AreaEditorValues;
}>;

export function AreaEditorForm(props: AreaEditorFormProps) {
  const {
    initial, submitting, locked, cancelLabel, error,
    listDirectory, nameInputRef, submitLabel, onCancel, onSubmit,
  } = props;
  const folderId = `${useId()}-default-folder`;
  const [name, setName] = useState(initial.name);
  const [cwd, setCwd] = useState(initial.defaultCwd ?? '');
  const [browsing, setBrowsing] = useState(false);
  const dialog = useDialogView();
  const normalizedName = name.trim();
  useEffect(() => {
    if (!dialog || !browsing) return;
    const cancel = () => setBrowsing(false);
    return dialog.pushView({
      title: 'Choose a directory',
      onEscape: cancel,
      body: (
        <DirectoryBrowser
          listDirectory={listDirectory}
          initialPath={cwd === '' ? null : cwd}
          mode="directory"
          onCancel={cancel}
          onSelect={(path) => { setCwd(path); setBrowsing(false); }}
        />
      ),
    });
  // eslint-disable-next-line react-hooks/exhaustive-deps -- capture the path and ports once when browsing opens; selection must not push a second view.
  }, [browsing, dialog]);

  const submit = (event: FormEvent<HTMLElement>) => {
    event.preventDefault();
    if (submitting || normalizedName === '') return;
    // Keep focus inside the modal when the request disables every field: on Enter
    // from Name the browser would otherwise leave the busy Dialog on <body>.
    event.currentTarget.querySelector<HTMLButtonElement>('button[type="submit"]')?.focus();
    onSubmit({
      name: normalizedName,
      defaultCwd: cwd === '' ? null : cwd,
    });
  };

  return (
    <VStack as="form" gap={3} className={styles.form} onSubmit={submit}>
      {error !== null && <Banner status="error" title={error} data-nc-area-editor-error />}
      <TextInput
        {...NATIVE_REQUIRED_INPUT_PROPS}
        ref={nameInputRef}
        label="Name"
        value={name}
        onChange={setName}
        width="100%"
        isDisabled={submitting || locked}
      />
      <HStack gap={1} align="center" className={styles.controlRow}>
        <HStack gap={1} align="center" className={styles.pills}>
          <FolderPill
            buttonId={folderId}
            value={cwd}
            controlLabel="Default folder"
            clearLabel="Use a new Neige workspace"
            isDisabled={submitting || locked}
            onBrowse={() => setBrowsing(true)}
            onClear={() => setCwd('')}
          />
        </HStack>
        <HStack gap={1} justify="end" className={styles.actions}>
          <Button type="button" variant="ghost" label={cancelLabel} isDisabled={submitting} onClick={onCancel} />
          <Button
            type="submit"
            variant="primary"
            label={submitting ? 'Saving…' : submitLabel}
            isDisabled={normalizedName === ''}
            isLoading={submitting}
            tooltip={submitting
              ? 'Saving in progress'
              : normalizedName === '' ? 'Enter a name to save' : submitLabel}
          />
        </HStack>
      </HStack>
    </VStack>
  );
}
