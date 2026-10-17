/** Read-only projection of the existing verified whoami owner. No probe or authority is issued here. */
import { createContext, useContext, useMemo, type ReactNode } from 'react';
import type { SessionIdentity } from '../../../../core/api/auth.ts';
type CurrentUserIdentity=Readonly<Pick<SessionIdentity,'userId'|'displayName'>>;
const CurrentUser = createContext<CurrentUserIdentity | null>(null);
export function CurrentUserScope({ value, children }: { value: SessionIdentity; children: ReactNode }) {
  const current=useMemo(()=>({userId:value.userId,displayName:value.displayName}),[value.userId,value.displayName]);
  return <CurrentUser.Provider value={current}>{children}</CurrentUser.Provider>;
}
export function useCurrentUser(): CurrentUserIdentity | null { return useContext(CurrentUser); }
