import { useEffect } from 'react';
import { parseGuideCatalog } from '../../ui/floating-control/public.ts';
import { useState } from '../../ui/state/public.ts';
import type { UiPreferences } from '../providers/ui-preferences.tsx';
import definition from './workspace-guides.json';

export function workspaceGuideFacts(hasProjects: boolean, hasWork: boolean, work: { closedAt: number | null } | undefined = undefined, settings = false) {
  return { 'has-projects': hasProjects, 'no-projects': !hasProjects, 'has-work': hasWork, 'routed-work': work !== undefined, 'work-open': work?.closedAt === null, 'work-closed': work !== undefined && work.closedAt !== null, 'settings-open': settings, demo: false, 'demo-revised': false };
}

type GuideSession = Readonly<{ acceptedScope: string | null; decision: boolean | null; epoch: number; resetPending: boolean; seen: ReadonlySet<string> }>;

/** Application-owned copy and enrollment; the UI engine knows no project policy. */
export function useWorkspaceGuide(preferences: UiPreferences, requested: boolean, empty: boolean, ready: boolean) {
  const [catalog] = useState(() => parseGuideCatalog(definition));
  const scope = preferences.guideScope();
  const [session, setSession] = useState<GuideSession>(() => ({ acceptedScope: null, decision: requested ? true : null, epoch: 0, resetPending: requested, seen: new Set<string>() }));
  // A cached render identity is not an accepted visit: only the commit acknowledgement confirms it.
  const changed = scope !== null && session.acceptedScope !== null && session.acceptedScope !== scope;
  const current = !changed ? session
    : { acceptedScope: null, decision: requested ? true : null, epoch: session.epoch + 1, resetPending: requested, seen: new Set<string>() };
  if (current !== session) setSession(current);
  const stored = preferences.guideEnabled();
  // The preference owner atomically accepts the live scope before acknowledging this plan.
  useEffect(() => {
    if (scope === null) return;
    const committed = preferences.commitGuideProgress(scope, {
      version: catalog.version,
      reset: current.resetPending ? catalog.tours.flatMap(tour => tour.steps.map(step => step.id)) : [],
      seen: [...current.seen],
      enabled: current.decision ?? (ready && stored === null ? empty : null),
    });
    if (committed) setSession(value => {
      if (value.epoch !== current.epoch || (value.acceptedScope === scope && !value.resetPending)) return value;
      return { ...value, acceptedScope: scope, resetPending: false };
    });
  }, [ready, empty, stored, scope, current.decision, current.seen, current.resetPending, current.epoch, catalog, preferences]);
  const start = () => {
    setSession({ acceptedScope: null, decision: true, epoch: current.epoch + 1, resetPending: true, seen: new Set() });
  };
  return {
    catalog, enabled: current.decision ?? stored === true, sessionKey: String(current.epoch),
    start,
    skip: () => setSession(value => ({ ...value, decision: false })),
    isSeen: (id: string) => current.seen.has(id) || (!current.resetPending && preferences.guideStepSeen(catalog.version, id)),
    onSeen: (id: string) => {
      setSession(value => ({ ...value, seen: new Set([...value.seen, id]) }));
    },
  };
}
