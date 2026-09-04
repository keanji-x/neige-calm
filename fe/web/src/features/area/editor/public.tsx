// One editor for both Area creation and Area settings. It is intentionally a
// pure form: the shell owns the Dialog, API mutations, template read and
// directory transport, so this feature can preserve drafts across a failed
// write without importing app composition.

import { Banner } from '@astryxdesign/core/Banner';
import { Button } from '@astryxdesign/core/Button';
import { HStack } from '@astryxdesign/core/HStack';
import { TextInput } from '@astryxdesign/core/TextInput';
import { VStack } from '@astryxdesign/core/VStack';
import { useEffect, useId, type FormEvent, type RefObject } from 'react';

import type { TrackTemplate } from '../../../../../core/domain/track.ts';
import { DirectoryBrowser, type ListDirectory } from '../../../ui/directory-browser/public.tsx';
import { useDialogView } from '../../../ui/dialog/public.tsx';
import { useState } from '../../../ui/state/public.ts';
import {
  FolderPill, NO_TEMPLATE_ID, TemplatePill,
} from '../default-pills/public.tsx';
import styles from './area-editor.module.css';

// Astryx forwards unknown base props to its native input, but its public type
// does not expose the HTML `required` constraint. Keep that semantic without
// turning on `isRequired`, whose visible “· Required” duplicates this compact
// dialog's only field label.
const NATIVE_REQUIRED_INPUT_PROPS = Object.freeze({ required: true });

export type AreaEditorValues = Readonly<{
  name: string;
  defaultTemplateId: string | null;
  defaultCwd: string | null;
}>;

export type AreaEditorPatch = Readonly<{
  name?: string;
  defaultTemplateId?: string | null;
  defaultCwd?: string | null;
}>;

export type AreaEditorFormProps = Readonly<{
  initial: AreaEditorValues;
  submitting: boolean;
  error: string | null;
  templates: readonly TrackTemplate[];
  templatesLoaded: boolean;
  templatesError: string | null;
  listDirectory: ListDirectory;
  nameInputRef: RefObject<HTMLInputElement | null>;
  submitLabel: string;
  onCancel: () => void;
  onSubmit: (values: AreaEditorValues) => void;
}>;

export function AreaEditorForm({
  initial, submitting, error, templates, templatesLoaded, templatesError,
  listDirectory, nameInputRef, submitLabel, onCancel, onSubmit,
}: AreaEditorFormProps) {
  const folderId = `${useId()}-default-folder`;
  const [name, setName] = useState(initial.name);
  const [templateId, setTemplateId] = useState(initial.defaultTemplateId ?? NO_TEMPLATE_ID);
  const [cwd, setCwd] = useState(initial.defaultCwd ?? '');
  const [browsing, setBrowsing] = useState(false);
  const dialog = useDialogView();
  const normalizedName = name.trim();
  const knownTemplate = templateId === NO_TEMPLATE_ID
    || templates.some((template) => template.id === templateId);
  const templateNotice = templatesError !== null
    ? templateId === NO_TEMPLATE_ID
      ? initial.defaultTemplateId === null
        ? `${templatesError} You can still save without a template.`
        : `${templatesError} Saving now will clear the Area’s default template.`
      : templateId === initial.defaultTemplateId
        ? `${templatesError} The saved default is preserved.`
        : `${templatesError} Your new selection will still be saved.`
    : templatesLoaded && !knownTemplate
      ? 'This saved template is not available in this build.'
      : null;

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
    // Keep focus inside the modal when the request flips every editable field
    // to disabled. This matters for Enter from Name: without the explicit move,
    // the browser unfocuses that input and leaves the busy Dialog on <body>.
    event.currentTarget.querySelector<HTMLButtonElement>('button[type="submit"]')?.focus();
    onSubmit({
      name: normalizedName,
      defaultTemplateId: templateId === NO_TEMPLATE_ID ? null : templateId,
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
        isDisabled={submitting}
      />
      <HStack gap={1} align="center" className={styles.controlRow}>
        <HStack gap={1} align="center" className={styles.pills}>
          <TemplatePill
            templates={templates}
            templatesLoaded={templatesLoaded}
            value={templateId}
            onChange={setTemplateId}
            placement="below"
            controlLabel="Default template"
            isDisabled={submitting}
          />
          <FolderPill
            buttonId={folderId}
            value={cwd}
            controlLabel="Default folder"
            clearLabel="Use a new Neige workspace"
            isDisabled={submitting}
            onBrowse={() => setBrowsing(true)}
            onClear={() => setCwd('')}
          />
        </HStack>
        <HStack gap={1} justify="end" className={styles.actions}>
          <Button type="button" variant="ghost" label="Cancel" isDisabled={submitting} onClick={onCancel} />
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
      {templateNotice !== null && <p className={styles.notice} role="status">{templateNotice}</p>}
    </VStack>
  );
}
