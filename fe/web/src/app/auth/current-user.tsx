/** Read-only projections of verified whoami. No probe or authority is issued here. */
import { createContext, useContext, useMemo, type ReactNode } from 'react';
import type { SessionIdentity } from '../../../../core/api/auth.ts';

type CurrentUserIdentity = Readonly<Pick<SessionIdentity, 'userId' | 'displayName'>>;
const CurrentUser = createContext<CurrentUserIdentity | null>(null);
const CurrentRole = createContext<string | null>(null);

export function CurrentUserScope({ value, children }: { value: SessionIdentity; children: ReactNode }) {
  const current = useMemo(() => ({ userId: value.userId, displayName: value.displayName }), [value.userId, value.displayName]);
  return <CurrentUser.Provider value={current}><CurrentRole.Provider value={value.role}>{children}</CurrentRole.Provider></CurrentUser.Provider>;
}

export function useCurrentUser(): CurrentUserIdentity | null { return useContext(CurrentUser); }
export function useCurrentRole(): string | null { return useContext(CurrentRole); }
