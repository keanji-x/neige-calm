import { TemplatePluginGuides } from './template-plugin-guides.tsx';
import { useQuery } from '@tanstack/react-query';
import { useCallback, useEffect, useRef } from 'react';
import type { ApiTransportPort } from '../../../../core/api/types.ts';
import type { UnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { templateDetailOperation } from '../../../../core/domain/template.ts';
import { availabilityOf } from '../../../../core/domain/agent-providers.ts';
import { folderConflictMessage } from '../../../../core/domain/area.ts';
import { ApiError, classifyFailure, NotSentError, refusedText, writeFailureOf } from '../../../../core/domain/failure-class.ts';
import { isBlankForKernel, TRACK_CREATE_FAILURES, TRACK_CREATE_TEXT, type NewTrackBodyWithoutFirstMessage } from '../../../../core/domain/track.ts';
import { ModelPill } from '../../features/chat/thread/model-pill.tsx';
import { useMentionTrigger } from '../../features/chat/thread/mention-trigger.tsx';
import { NewTrackForm, type NewTrackDraft, type NewTrackFormState } from '../../features/area/new-track/public.tsx';
import { useCompactViewport } from '../../ui/viewport/public.ts';
import { ErrorBox } from '../../ui/error-box/public.tsx';
import { agentProvidersQueryOptions } from '../providers/agent-providers.ts';
import { createDirectoryLister } from '../providers/directory.ts';
import { useMentionSearch } from '../providers/mentions.ts';
import { runOperation, folderConflictOf, modelCatalogQueryOptions, useTrackMutations, useTrackRecipes, useTrackTemplates, useWorkspace, type Workspace } from '../providers/queries.ts';
import { readHostThemeRgb } from '../theme/host-rgb.ts';
import { mintIdempotencyKey } from '../providers/idempotency-key.ts';
import { useGo, useRouteParam } from './navigation.ts';
import { useNewTrackSession, type NewTrackSession, type TrackCreationRequest } from './new-track-drafts.tsx';

type RouteProps = { transport: ApiTransportPort; unauthorized: UnauthorizedChannel };

export function NewTrackRoute({ transport, unauthorized }: RouteProps) {
  const areaId = useRouteParam('/area/');
  const workspace = useWorkspace(transport, unauthorized);
  const area = workspace.areas.find((candidate) => candidate.id === areaId);
  const { store, session } = useNewTrackSession(areaId ?? '', area);
  const go = useGo();
  if (session === null) {
    if (workspace.areasLoading) return null;
    if (workspace.areasError !== null) return <ErrorBox message={workspace.areasError.message} onRetry={workspace.retryAreas} />;
    if (area !== undefined) return null; // The provider initializes the Area draft after this read.
    return <ErrorBox message="This area could not be found." onRetry={() => go({ name: 'today' })} />;
  }
  return <NewTrackEditor key={session.area.id} transport={transport} unauthorized={unauthorized}
    workspace={workspace} session={session} store={store} />;
}

function NewTrackEditor({ transport, unauthorized, workspace, session, store }: RouteProps & {
  workspace: Workspace;
  session: NewTrackSession;
  store: ReturnType<typeof useNewTrackSession>['store'];
}) {
  const compactViewport = useCompactViewport();
  const areaId = session.area.id;
  /* One group per provider, each following its provider's availability (#1817): `not_configured` leaves it
     out; `unavailable` shows the server's reason, and disables the group only where create refuses it
     (`CREATE_REFUSED_WHEN_UNAVAILABLE`: Claude). */
  const codexCatalog = useQuery(modelCatalogQueryOptions(transport, { kind: 'provider', provider: 'codex' }, unauthorized));
  const claudeCatalog = useQuery(modelCatalogQueryOptions(transport, { kind: 'provider', provider: 'claude' }, unauthorized));
  const availability = useQuery(agentProvidersQueryOptions(transport, unauthorized));
  const trackMutations = useTrackMutations(transport, unauthorized);
  const templates = useTrackTemplates(transport, unauthorized);
  const recipes = useTrackRecipes(transport, unauthorized);
  const go = useGo();
  const listDirectory = createDirectoryLister(transport, unauthorized);
  /* The sentence is the new track's Planner's first message, and that Planner reads this Area's reports; there is no track yet to rank first. */
  const mentionTrigger = useMentionTrigger(useMentionSearch(transport, unauthorized, areaId, null));
  const available = workspace.areasError === null && !workspace.areasLoading
    && workspace.areas.some((area) => area.id === areaId);
  const liveRef = useRef(true);
  useEffect(() => {
    liveRef.current = true;
    return () => { liveRef.current = false; };
  }, []);
  const saveForm = useCallback((form: NewTrackFormState) => { store.update(areaId, { form }); }, [areaId, store]);

  const openCreated = (trackId: string) => {
    store.forget(areaId);
    go({ name: 'track', trackId, openPlanner: true });
  };

  const submit = (draft: NewTrackDraft, authorization?: Readonly<{ folder_id: number; area_id: string }>, replacementKey?: string) => {
    // The provider owns the lease too: leaving and returning during a POST
    // must not enable a second request before the first settles.
    const current = store.get(areaId);
    // After `key-reused` only an explicit new track goes anywhere: a retry under the key cannot.
    if (current === null || current.creating || !available || current.createdTrackId !== null
      || current.canRetryAsNewTrack) return;
    const hadUnconfirmedRequest = replacementKey === undefined && current.request !== null;
    const attemptKey = replacementKey ?? current.key;
    const body = {
      area_id: areaId,
      planner_provider: current.provider,
      theme: readHostThemeRgb(),
      ...(current.model.model === null ? {} : { model: current.model.model }),
      ...(current.model.reasoning_effort === null ? {} : { reasoning_effort: current.model.reasoning_effort }),
      ...(draft.template_id === undefined ? {} : { template_id: draft.template_id }),
      ...(draft.template_input === undefined ? {} : { template_input: draft.template_input }),
      ...(draft.recipe_id === undefined ? {} : { recipe_id: draft.recipe_id }),
      ...(draft.cwd === undefined ? {} : { cwd: draft.cwd, attach_folder: true }),
      ...(authorization === undefined ? {} : { allow_cross_area_cwd: authorization }),
    } satisfies NewTrackBodyWithoutFirstMessage;
    // The original wire body travels with the key. A retry after navigation,
    // theme changes or template refresh must not silently change its payload.
    const request: TrackCreationRequest = replacementKey === undefined && current.request !== null
      ? current.request
      : { body: isBlankForKernel(draft.message) ? body : { ...body, first_message: draft.message }, key: attemptKey };
    store.update(areaId, { creating: true, request, error: null, canRetryAsNewTrack: false, folderConflict: null });
    const creation = trackMutations.create(request.body, request.key);
    void creation.then((track) => {
      // A late acknowledgement belongs to its draft; it never steals the
      // navigation the reader made while waiting. Returning offers Open track.
      store.update(areaId, { createdTrackId: track.id });
      if (liveRef.current) openCreated(track.id);
    }).catch((failure: unknown) => {
      const conflict = folderConflictOf(failure);
      if (conflict !== null) {
        const owner = workspace.areas.find((area) => area.id === conflict.area_id);
        store.update(areaId, {
          request: hadUnconfirmedRequest ? request : null,
          error: folderConflictMessage(conflict, owner?.name ?? null),
          folderConflict: !hadUnconfirmedRequest && owner !== undefined && owner.id !== areaId
            && conflict.conflict_kind !== 'ancestor' && draft.cwd !== undefined
            ? { ownerAreaId: owner.id, areaName: owner.name, folderId: conflict.folder_id, cwd: draft.cwd } : null,
        });
        return;
      }
      const kind = classifyFailure(failure instanceof ApiError ? failure.failure : null, TRACK_CREATE_FAILURES);
      const rejectedBeforeDispatch = failure instanceof NotSentError;
      store.update(areaId, {
        // Not sent leaves an earlier unconfirmed create as it was; an unknown answer is the fixed state, never its text.
        error: rejectedBeforeDispatch ? (hadUnconfirmedRequest ? TRACK_CREATE_TEXT.unknown : TRACK_CREATE_TEXT.refused)
          : kind === 'unconfirmed' ? TRACK_CREATE_TEXT.unknown : refusedText(writeFailureOf(failure), TRACK_CREATE_TEXT.refused),
        ...(kind === 'exhausted' ? { key: mintIdempotencyKey(), request: null } : {}),
        // A refused retry says nothing about the earlier attempt's outcome.
        ...(!hadUnconfirmedRequest && (rejectedBeforeDispatch || kind === 'rejected') ? { request: null } : {}),
        canRetryAsNewTrack: kind === 'key-reused',
      });
    }).finally(() => { store.update(areaId, { creating: false }); });
  };

  const retryAsNewTrack = () => {
    store.update(areaId, { key: mintIdempotencyKey(), request: null, canRetryAsNewTrack: false, error: null });
  };
  const folderConflict = session.folderConflict;
  const recoverFolderConflict = (draft: NewTrackDraft) => {
    if (folderConflict === null || draft.cwd !== folderConflict.cwd) return;
    const key = mintIdempotencyKey();
    store.update(areaId, { key, request: null });
    submit(draft, {
      folder_id: folderConflict.folderId,
      area_id: folderConflict.ownerAreaId,
    }, key);
  };
  const areaFailure = workspace.areasError !== null
    ? `Areas could not be refreshed. Your draft is kept here. ${workspace.areasError.message}`
    : !available ? 'Area unavailable. Your draft is kept here; select and copy it to use elsewhere.' : null;
  const createdTrackId = session.createdTrackId;
  const configurationLocked = session.creating || session.request !== null || !available || createdTrackId !== null;
  const loadTemplate = useCallback((id: string) => runOperation(transport, templateDetailOperation(id), unauthorized), [transport, unauthorized]);

  return <NewTrackForm
    mentionTrigger={mentionTrigger}
    templatePluginGuides={id => <TemplatePluginGuides key={id} templateId={id} transport={transport} unauthorized={unauthorized} />}
    modelControls={
      <ModelPill
        groups={[
          { provider: 'codex', catalog: codexCatalog.data ?? null, availability: availabilityOf(availability.data, 'codex') },
          { provider: 'claude', catalog: claudeCatalog.data ?? null, availability: availabilityOf(availability.data, 'claude') },
        ]}
        provider={session.provider}
        effortControl={compactViewport ? 'in-menu' : 'separate'}
        selection={session.model}
        /* The pick is the Planner's provider too (#1810); a pick never carries another provider's effort. */
        onChange={(model, provider) => { store.update(areaId, { provider, model }); }}
        isDisabled={configurationLocked}
      />
    }
    initialDraft={session.form ?? undefined}
    onDraftChange={saveForm}
    submitBlocked={!available || createdTrackId !== null || session.canRetryAsNewTrack}
    locked={session.request !== null}
    submitting={session.creating}
    error={areaFailure ?? (createdTrackId !== null ? 'Your track was created while you were away.' : session.error)}
    templates={templates.templates}
    loadTemplate={loadTemplate}
    templatesLoaded={templates.loaded}
    templatesError={templates.error}
    recipes={recipes.recipes}
    errorAction={createdTrackId !== null && session.request !== null
      ? { label: 'Open track', onClick: () => { openCreated(createdTrackId); } }
      : folderConflict !== null
        ? { label: `Reuse directory in ${session.area.name}`, isApplicable: (draft) => draft.cwd === folderConflict.cwd, onClick: recoverFolderConflict }
        : session.canRetryAsNewTrack ? { label: 'Start as a new track', onClick: retryAsNewTrack } : undefined}
    onManageRecipes={() => go({ name: 'recipes' })}
    initialTemplateId={session.area.defaultTemplateId}
    initialCwd={session.area.defaultCwd}
    listDirectory={listDirectory}
    onSubmit={submit}
  />;
}
