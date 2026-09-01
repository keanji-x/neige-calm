// Settings › Templates — the wave-template list and one template's editor.
//
// Two presentational surfaces, both props-driven like `SettingsPage`: they
// never call an API, and navigation leaves through callbacks (INV-A11Y-061 —
// no `<a href>` anywhere, so a row that navigates is a `<button>`).
//
// ## Why a drill-in and not three cards on the Settings page
//
// The first cut stacked every template's whole task list inline under a
// "Wave templates" heading. Three templates today; the list is meant to grow,
// and every task of every template was on screen at once — Settings became
// mostly templates. A list that names each template and drills in keeps
// Settings readable at any number of templates, and it makes the editor a real
// place: Back leaves the editor rather than leaving Settings, and one
// template's editor can be linked to.
//
// ## What this editor deliberately does NOT offer
//
// **No rename, no delete, no reorder.** A template's tasks live in the template
// wave's report, so `wave_report_edit_guard` (#1179) governs them: a task `key`
// is immutable for the life of its block, and a live task may only leave a
// document as a tombstone that `prepare_fork_report` then copies into every
// wave forked afterwards. Both come back from the server as a 400.
//
// A control whose only possible outcome is an error is worse than an absent
// one, so the affordances are absent and the reason is stated on the page
// instead of discovered by failing. Lifting that limit means changing the
// guard — argued in `routes/wave_templates.rs`, and its own slice.
//
// **Whole task objects round-trip.** The editor reads `key` and `goal`; every
// other field on a task — `acceptance_criteria`, `context`, `gate`,
// `depends_on`, `no_gate_reason` — is opaque cargo it must hand back
// untouched, because the server stores exactly what it is given.

import type { WaveTemplate, WaveTemplateGoalEdit } from '../../../../core/domain/wave.ts';
import { Breadcrumb, PageHeader, PageTitle } from '../../ui/page-header/public.tsx';
import { useState } from '../../ui/state/public.ts';
import { Banner } from '@astryxdesign/core/Banner';
import { Button } from '@astryxdesign/core/Button';
import { Heading } from '@astryxdesign/core/Heading';
import { HStack } from '@astryxdesign/core/HStack';
import { Text } from '@astryxdesign/core/Text';
import { TextInput } from '@astryxdesign/core/TextInput';
import { VStack } from '@astryxdesign/core/VStack';
import styles from './settings.module.css';

/**
 * What a template save sends back up: a **diff**, never a task list.
 *
 * The editor states `key` and `goal` and nothing else. Every other field of a
 * task block belongs to the server — review round 2 measured what happens
 * otherwise (`released_by_user: true` and `spawn: "sub-wave"` were accepted and
 * stored, and omitting a tombstone erased it).
 */
export type TemplateSave = Readonly<{
  id: string;
  title: string;
  edits: readonly WaveTemplateGoalEdit[];
  appends: readonly WaveTemplateGoalEdit[];
}>;

export type TemplateListProps = Readonly<{
  /** `undefined` means "still loading" — never render an empty list for it. */
  templates: readonly WaveTemplate[] | undefined;
  loadError: string | null;
  onRetryLoad: () => void;
  onOpenSettings: () => void;
  onEdit: (templateId: string) => void;
}>;

export function TemplateListPage({
  templates, loadError, onRetryLoad, onOpenSettings, onEdit,
}: TemplateListProps) {
  return (
    <div className={styles.page}>
      <PageHeader
        breadcrumb={<Breadcrumb ancestor="Settings" onNavigate={onOpenSettings} />}
        title={<PageTitle>Templates</PageTitle>}
      />
      <VStack className={styles.form} gap={4} align="stretch">
        <Text as="p" color="secondary">
          What a new wave starts from. Editing a template changes every wave created
          from it afterwards; waves already created keep the plan they were forked with.
        </Text>
        {loadError !== null && (
          <VStack gap={2} align="start">
            <Banner status="error" title={loadError} role="alert" />
            <Button type="button" variant="secondary" label="Retry" onClick={onRetryLoad} />
          </VStack>
        )}
        {templates === undefined
          ? loadError === null && <Text as="p" color="secondary">Loading templates…</Text>
          : (
            <ul className={styles.templateList}>
              {templates.map((template) => (
                <li key={template.id} className={styles.templateRow}>
                  <HStack gap={3} align="center" justify="between">
                    <VStack gap={0} align="start">
                      <Text as="span">{template.title}</Text>
                      {/* The count, not the task list: the list is what the
                          next screen is for, and repeating it here was what
                          made the old inline version unreadable. */}
                      <Text as="span" color="secondary">
                        {template.tasks.length === 1 ? '1 task' : `${template.tasks.length} tasks`}
                      </Text>
                    </VStack>
                    <Button
                      type="button"
                      variant="secondary"
                      label="Edit"
                      // The accessible name has to say *which* template; three
                      // buttons all named "Edit" is a list a screen reader
                      // cannot navigate.
                      aria-label={`Edit ${template.title}`}
                      onClick={() => onEdit(template.id)}
                    />
                  </HStack>
                </li>
              ))}
            </ul>
          )}
      </VStack>
    </div>
  );
}

export type TemplateEditorProps = Readonly<{
  /** `undefined` while the definition is still loading. */
  template: WaveTemplate | undefined;
  loadError: string | null;
  onRetryLoad: () => void;
  saving: boolean;
  saveError: string | null;
  savedAt: number | null;
  onSave: (save: TemplateSave) => void | Promise<void>;
  onOpenTemplates: () => void;
}>;

type TemplateDraft = {
  title: string;
  /** Goals for the tasks that already exist, in the order they are shown. */
  tasks: WaveTemplateGoalEdit[];
  /** Tasks the user added and has not saved yet. */
  appends: WaveTemplateGoalEdit[];
};

export function TemplateEditorPage({
  template, loadError, onRetryLoad, saving, saveError, savedAt, onSave, onOpenTemplates,
}: TemplateEditorProps) {
  const incoming: TemplateDraft | null = template === undefined
    ? null
    : { title: template.title, tasks: template.tasks.map((task) => ({ ...task })), appends: [] };
  // Seed by serialized *value*, not object identity: the query cache hands back
  // an equal-but-new object on every render and must not wipe out what the user
  // is typing. A genuine server change does re-seed.
  const incomingKey = incoming === null ? null : JSON.stringify(incoming);

  const [seed, setSeed] = useState<string | null>(null);
  const [draft, setDraft] = useState<TemplateDraft | null>(null);
  if (incoming !== null && incomingKey !== seed) {
    // `seed` advances only when the draft actually takes the new value, so a
    // definition that lands mid-save is applied on the next render rather than
    // being recorded as seen and lost. Advancing it unconditionally wedged the
    // editor permanently (review round 1).
    if (seed === null || !saving) {
      setSeed(incomingKey);
      setDraft(incoming);
    }
  }

  const dirty = draft !== null && JSON.stringify(draft) !== incomingKey;
  // The server refuses a blank title and a blank goal unconditionally, so
  // offering Save in those states would be an affordance whose only outcome is
  // a 400 — the same rule `NewTaskRow` applies to a malformed key.
  const blankTitle = draft !== null && draft.title.trim() === '';
  const blankGoal = draft !== null
    && [...draft.tasks, ...draft.appends].some((task) => task.goal.trim() === '');
  const submittable = dirty && !blankTitle && !blankGoal;

  return (
    <div className={styles.page}>
      <PageHeader
        breadcrumb={<Breadcrumb ancestor="Templates" onNavigate={onOpenTemplates} />}
        title={<PageTitle>{template?.title ?? 'Template'}</PageTitle>}
      />
      <VStack className={styles.form} gap={4} align="stretch">
        {loadError !== null && (
          <VStack gap={2} align="start">
            <Banner status="error" title={loadError} role="alert" />
            <Button type="button" variant="secondary" label="Retry" onClick={onRetryLoad} />
          </VStack>
        )}
        {draft === null
          ? loadError === null && <Text as="p" color="secondary">Loading template…</Text>
          : (
            <>
              <TextInput
                label="Title"
                value={draft.title}
                width="100%"
                status={blankTitle
                  ? { type: 'error', message: 'A template needs a title — the New wave dialog lists templates by it.' }
                  : undefined}
                onChange={(value: string) => setDraft({ ...draft, title: value })}
              />

              <Heading level={2}>Tasks</Heading>
              {/* Stated, not discovered by failing — see this file's header. */}
              <Text as="p" color="secondary">
                A task&rsquo;s key is fixed once the template exists, and a task cannot be
                removed. You can reword any task and add new ones.
              </Text>

              {draft.tasks.map((task, index) => (
                <TextInput
                  key={task.key}
                  label={task.key}
                  value={task.goal}
                  width="100%"
                  status={task.goal.trim() === '' ? { type: 'error', message: 'A task needs a goal.' } : undefined}
                  onChange={(value: string) => setDraft({
                    ...draft,
                    tasks: draft.tasks.map((entry, position) =>
                      position === index ? { ...entry, goal: value } : entry),
                  })}
                />
              ))}

              {draft.appends.map((task, index) => (
                <TextInput
                  key={`new-${task.key}`}
                  label={`${task.key} (new)`}
                  value={task.goal}
                  width="100%"
                  status={task.goal.trim() === '' ? { type: 'error', message: 'A task needs a goal.' } : undefined}
                  onChange={(value: string) => setDraft({
                    ...draft,
                    appends: draft.appends.map((entry, position) =>
                      position === index ? { ...entry, goal: value } : entry),
                  })}
                />
              ))}

              <NewTaskRow
                existingKeys={[...draft.tasks, ...draft.appends].map((task) => task.key)}
                onAdd={(key, goal) => setDraft({ ...draft, appends: [...draft.appends, { key, goal }] })}
              />

              {saveError !== null && <Banner status="error" title={saveError} role="alert" />}
              <HStack gap={2} align="center">
                <Button
                  type="button"
                  variant="primary"
                  label={saving ? 'Saving…' : 'Save'}
                  isDisabled={!submittable && !saving}
                  // Busy, not disabled — astryx renders a native `disabled`
                  // unless the button is interruptible, and focus is on this
                  // button at exactly this moment. The re-entry guard is the
                  // `if (saving) return` below, which has its own test.
                  isLoading={saving}
                  isInterruptible
                  onClick={() => {
                    if (saving || template === undefined || incoming === null) return;
                    // Only the goals that actually changed. Sending every task
                    // would make a save re-assert values nobody edited, which
                    // is the same defect INV-SETTINGS-001 removes for settings.
                    const edits = draft.tasks.filter((task, index) =>
                      task.goal !== incoming.tasks[index]?.goal);
                    void onSave({ id: template.id, title: draft.title, edits, appends: draft.appends });
                  }}
                />
                <Button
                  type="button"
                  variant="secondary"
                  label="Reset"
                  // Reset has no in-flight state of its own, so a real
                  // `disabled` is right here — unlike Save, focus is not on it
                  // at the moment a save starts.
                  isDisabled={!dirty || saving}
                  onClick={() => { if (!saving && incoming !== null) setDraft(incoming); }}
                />
                {savedAt !== null && !dirty && (
                  <span className={styles.saved} role="status">Saved.</span>
                )}
              </HStack>
            </>
          )}
      </VStack>
    </div>
  );
}

/**
 * Appending a task.
 *
 * Local to the row rather than a blank entry pushed into the draft: an empty
 * task in `draft.tasks` would make the form dirty before the user has typed
 * anything, and would be sent to a server that rejects a blank goal. The key is
 * validated here for the same reason the server validates it — a key that
 * `key_is_valid` refuses, or one already in the list, can only 400.
 */
function NewTaskRow({ existingKeys, onAdd }: {
  existingKeys: readonly string[];
  onAdd: (key: string, goal: string) => void;
}) {
  const [key, setKey] = useState('');
  const [goal, setGoal] = useState('');
  const trimmedKey = key.trim();
  const trimmedGoal = goal.trim();
  // Mirrors `report_blocks::tasks::key_is_valid`. Duplicated shapes drift, so
  // this is the *client* half of a check the server still performs — it exists
  // to keep the user out of a round trip, never as the authority.
  const keyShapeOk = /^[a-z0-9]+(?:-[a-z0-9]+)*$/.test(trimmedKey);
  const duplicate = existingKeys.includes(trimmedKey);
  const status = trimmedKey === '' || (keyShapeOk && !duplicate)
    ? undefined
    : {
      type: 'error' as const,
      message: duplicate
        ? 'That key is already used by another task in this template.'
        : 'Use lowercase letters, digits and single hyphens — for example `run-tests`.',
    };

  return (
    <VStack gap={2} align="stretch" className={styles.newTask}>
      <Heading level={3}>Add a task</Heading>
      <TextInput
        label="Key"
        value={key}
        width="100%"
        status={status}
        onChange={(value: string) => setKey(value)}
      />
      <TextInput
        label="Goal"
        value={goal}
        width="100%"
        onChange={(value: string) => setGoal(value)}
      />
      <HStack gap={2}>
        <Button
          type="button"
          variant="secondary"
          label="Add task"
          isDisabled={trimmedKey === '' || trimmedGoal === '' || !keyShapeOk || duplicate}
          onClick={() => {
            onAdd(trimmedKey, trimmedGoal);
            setKey('');
            setGoal('');
          }}
        />
      </HStack>
    </VStack>
  );
}
