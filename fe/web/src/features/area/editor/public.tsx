// One editor for both Area creation and Area settings; a pure form, the shell owns
// the Dialog, mutations, template read and directory transport.

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
import { GuideAnchor, useGuideTargetActive } from '../../../ui/floating-control/public.ts';

// Astryx's public type does not expose the native `required` constraint, and
// `isRequired` would duplicate this dialog's only field label with "· Required".
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
  /** An unconfirmed creation must retry its original payload. */
  locked: boolean;
  cancelLabel: string;
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
  initial, submitting, locked, cancelLabel, error, templates, templatesLoaded, templatesError,
  listDirectory, nameInputRef, submitLabel, onCancel, onSubmit,
}: AreaEditorFormProps) {
  const folderId = `${useId()}-default-folder`;
  const guidingFolder = useGuideTargetActive('area-defaults');
  const guidingTemplate = useGuideTargetActive('area-template');
  const guiding = guidingFolder || guidingTemplate;
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
    // Keep focus inside the modal when the request disables every field: on Enter
    // from Name the browser would otherwise leave the busy Dialog on <body>.
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
      <GuideAnchor id="area-name" reserveSpace canAdvance={normalizedName !== ''} disabled={initial.name !== '' || browsing || submitting || locked}
        onExample={name.trim() !== '' ? undefined : (text) => { setName(text); nameInputRef.current?.focus(); }}>
      {(anchor, describedBy) => <div ref={anchor} aria-describedby={describedBy} tabIndex={-1}><TextInput
        {...NATIVE_REQUIRED_INPUT_PROPS}
        ref={nameInputRef}
        label="Name"
        value={name}
        onChange={setName}
        width="100%"
        isDisabled={submitting || locked}
      /></div>}</GuideAnchor>
      <HStack gap={1} align="center" className={`${styles.controlRow} ${guiding ? styles.guided : ''}`}>
        <HStack gap={1} align="center" className={styles.pills}>
          <GuideAnchor id="area-template" reserveSpace canAdvance={templateId !== NO_TEMPLATE_ID} disabled={browsing || submitting || locked}>
          {(anchor, describedBy) => <span ref={anchor} aria-describedby={describedBy} tabIndex={-1}><TemplatePill
            templates={templates}
            templatesLoaded={templatesLoaded}
            value={templateId}
            onChange={setTemplateId}
            placement="below"
            controlLabel="Default template"
            isDisabled={submitting || locked}
          /></span>}</GuideAnchor>
          <GuideAnchor id="area-defaults" reserveSpace disabled={browsing || submitting || locked}>
          {(anchor, describedBy) => <FolderPill
            buttonId={folderId}
            buttonRef={anchor}
            describedBy={describedBy}
            value={cwd}
            controlLabel="Default folder"
            clearLabel="Use a new Neige workspace"
            isDisabled={submitting || locked}
            onBrowse={() => setBrowsing(true)}
            onClear={() => setCwd('')}
          />}
          </GuideAnchor>
        </HStack>
        <HStack gap={1} justify="end" className={styles.actions}>
          <Button type="button" variant="ghost" label={cancelLabel} isDisabled={submitting} onClick={onCancel} />
          <GuideAnchor id="area-create" disabled={normalizedName === '' || browsing || submitting || locked} placement="top">
          {(anchor, describedBy) => <Button ref={anchor} aria-describedby={describedBy}
            type="submit"
            variant="primary"
            label={submitting ? 'Saving…' : submitLabel}
            isDisabled={normalizedName === ''}
            isLoading={submitting}
            tooltip={submitting
              ? 'Saving in progress'
              : normalizedName === '' ? 'Enter a name to save' : submitLabel}
          />}</GuideAnchor>
        </HStack>
      </HStack>
      {templateNotice !== null && <p className={styles.notice} role="status">{templateNotice}</p>}
    </VStack>
  );
}
