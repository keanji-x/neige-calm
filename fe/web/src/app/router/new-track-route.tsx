import { useCallback, useEffect, useRef } from 'react';
import type { ApiTransportPort } from '../../../../core/api/types.ts';
import type { UnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { folderConflictMessage } from '../../../../core/domain/area.ts';
import { isBlankForKernel, trackCreateKeyAction, type NewTrackBodyWithoutFirstMessage } from '../../../../core/domain/track.ts';
import { NewTrackForm, type NewTrackDraft, type NewTrackFormState } from '../../features/area/new-track/public.tsx';
import { ErrorBox } from '../../ui/error-box/public.tsx';
import { createDirectoryLister } from '../providers/directory.ts';
import { ApiError, OfflineSubmissionError, folderConflictOf, useTrackMutations, useTrackRecipes, useTrackTemplates, useWorkspace, type Workspace } from '../providers/queries.ts';
import { readHostThemeRgb } from '../theme/host-rgb.ts';
import { mintIdempotencyKey } from './idempotency-key.ts';
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
  const areaId = session.area.id;
  const trackMutations = useTrackMutations(transport, unauthorized);
  const templates = useTrackTemplates(transport, unauthorized);
  const recipes = useTrackRecipes(transport, unauthorized);
  const go = useGo();
  const listDirectory = createDirectoryLister(transport, unauthorized);
  const available = workspace.areasError === null && !workspace.areasLoading
    && workspace.areas.some((area) => area.id === areaId);
  const liveRef = useRef(true);
  useEffect(() => {
    liveRef.current = true;
    return () => { liveRef.current = false; };
  }, []);
  const saveForm = useCallback((form: NewTrackFormState) => { store.update(areaId, { form }); }, [areaId, store]);

  const openCreated = (trackId: string, request: TrackCreationRequest) => {
    store.forget(areaId);
    go({ name: 'track', trackId, openPlanner: true,
      ...(request.body.first_message === undefined ? {} : { openPlannerMessage: request.body.first_message }) });
  };

  const submit = (draft: NewTrackDraft, targetAreaId = areaId, replacementKey?: string) => {
    // The provider owns the lease too: leaving and returning during a POST
    // must not enable a second request before the first settles.
    const current = store.get(areaId);
    if (current === null || current.creating || !available || current.createdTrackId !== null) return;
    const attemptKey = replacementKey ?? current.key;
    const body = {
      area_id: targetAreaId,
      theme: readHostThemeRgb(),
      ...(draft.template_id === undefined ? {} : { template_id: draft.template_id }),
      ...(draft.template_input === undefined ? {} : { template_input: draft.template_input }),
      ...(draft.recipe_id === undefined ? {} : { recipe_id: draft.recipe_id }),
      ...(draft.cwd === undefined ? {} : { cwd: draft.cwd, attach_folder: true }),
    } satisfies NewTrackBodyWithoutFirstMessage;
    // The original wire body travels with the key. A retry after navigation,
    // theme changes or template refresh must not silently change its payload.
    const request: TrackCreationRequest = replacementKey === undefined && current.request !== null
      ? current.request
      : isBlankForKernel(draft.message) ? { body }
        : { body: { ...body, first_message: draft.message }, key: attemptKey };
    store.update(areaId, { creating: true, request, error: null, canRetryAsNewTrack: false, folderConflict: null });
    const creation = request.key === undefined
      ? trackMutations.create(request.body)
      : trackMutations.create(request.body, request.key);
    void creation.then((track) => {
      // A late acknowledgement belongs to its draft; it never steals the
      // navigation the reader made while waiting. Returning offers Open track.
      store.update(areaId, { createdTrackId: track.id });
      if (liveRef.current) openCreated(track.id, request);
    }).catch((failure: unknown) => {
      const conflict = folderConflictOf(failure);
      if (conflict !== null) {
        const owner = workspace.areas.find((area) => area.id === conflict.area_id);
        store.update(areaId, {
          request: null,
          error: folderConflictMessage(conflict, owner?.name ?? null),
          folderConflict: owner !== undefined && owner.id !== targetAreaId
            && conflict.conflict_kind !== 'ancestor' && draft.cwd !== undefined
            ? { areaId: owner.id, areaName: owner.name, cwd: draft.cwd } : null,
        });
        return;
      }
      const keyAction = failure instanceof ApiError ? trackCreateKeyAction(failure.failure) : 'preserve';
      const rejectedBeforeDispatch = failure instanceof OfflineSubmissionError;
      const rejectedInput = failure instanceof ApiError && failure.failure.kind === 'http'
        && failure.failure.status >= 400 && failure.failure.status < 500
        && failure.failure.status !== 408 && failure.failure.status !== 409;
      store.update(areaId, {
        error: failure instanceof ApiError ? failure.message : 'Could not create the track.',
        ...(keyAction === 'replace' ? { key: mintIdempotencyKey(), request: null } : {}),
        ...(rejectedBeforeDispatch || rejectedInput ? { request: null } : {}),
        canRetryAsNewTrack: keyAction === 'offer-explicit-replace',
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
    submit(draft, folderConflict.areaId, key);
  };
  const areaFailure = workspace.areasError !== null
    ? `Areas could not be refreshed. Your draft is kept here. ${workspace.areasError.message}`
    : !available ? 'Area unavailable. Your draft is kept here; select and copy it to use elsewhere.' : null;
  const createdTrackId = session.createdTrackId;
  return <NewTrackForm
    initialDraft={session.form ?? undefined}
    onDraftChange={saveForm}
    submitBlocked={!available || createdTrackId !== null}
    locked={session.request !== null}
    submitting={session.creating}
    error={areaFailure ?? (createdTrackId !== null ? 'Your track was created while you were away.' : session.error)}
    templates={templates.templates}
    templatesLoaded={templates.loaded}
    templatesError={templates.error}
    recipes={recipes.recipes}
    errorAction={createdTrackId !== null && session.request !== null
      ? { label: 'Open track', onClick: () => { if (session.request !== null) openCreated(createdTrackId, session.request); } }
      : folderConflict !== null
        ? { label: `Create in ${folderConflict.areaName}`, isApplicable: (draft) => draft.cwd === folderConflict.cwd, onClick: recoverFolderConflict }
        : session.canRetryAsNewTrack ? { label: 'Start as a new track', onClick: retryAsNewTrack } : undefined}
    onManageRecipes={() => go({ name: 'recipes' })}
    initialTemplateId={session.area.defaultTemplateId}
    initialCwd={session.area.defaultCwd}
    listDirectory={listDirectory}
    onSubmit={submit}
  />;
}
