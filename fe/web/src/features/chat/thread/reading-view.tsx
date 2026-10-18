import { useCallback, useMemo, type RefObject } from 'react';
import { useState } from '../../../ui/state/public.ts';
import { noTranscriptGroupKeys, type TranscriptGroupKeys } from '../../../../../core/domain/conversation-groups.ts';
import { untouchedToolCallGroup, type ToolCallGroupUi } from './activity-groups.tsx';

/** Reading affordances belong to the thread, independently of its desktop/mobile surface. */
export type ThreadReadingView = Readonly<{
  groupKeys: RefObject<TranscriptGroupKeys>;
  groups: ReadonlyMap<string, ToolCallGroupUi>;
  expanded: ReadonlyMap<string, boolean>;
  updateGroup: (key: string, update: (previous: ToolCallGroupUi) => ToolCallGroupUi) => void;
  setExpanded: (key: string, expanded: boolean) => void;
}>;

function initialView(id: string | null) {
  return { id, groupKeys: { current: noTranscriptGroupKeys() }, groups: new Map<string, ToolCallGroupUi>(), expanded: new Map<string, boolean>() };
}

export function useThreadReadingView(id: string | null): ThreadReadingView {
  const [state, setState] = useState(() => initialView(id));
  const current = state.id === id ? state : initialView(id);
  if (current !== state) setState(current);
  const owner = current.groupKeys;
  const updateGroup = useCallback<ThreadReadingView['updateGroup']>((key, update) => {
    setState(previous => {
      if (previous.groupKeys !== owner) return previous;
      const before = previous.groups.get(key) ?? untouchedToolCallGroup();
      const after = update(before);
      return after === before ? previous : { ...previous, groups: new Map(previous.groups).set(key, after) };
    });
  }, [owner, setState]);
  const setExpanded = useCallback<ThreadReadingView['setExpanded']>((key, expanded) => {
    setState(previous => previous.groupKeys !== owner || (previous.expanded.get(key) ?? false) === expanded ? previous : { ...previous,
      expanded: new Map(previous.expanded).set(key, expanded) });
  }, [owner, setState]);
  return useMemo(() => ({ groupKeys: current.groupKeys, groups: current.groups, expanded: current.expanded,
    updateGroup, setExpanded }), [current, updateGroup, setExpanded]);
}
