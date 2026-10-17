import { cleanup, render, screen, waitFor } from '@testing-library/react';
import { QueryClient } from '@tanstack/react-query';
import { afterEach, expect, it, vi } from 'vitest';
import type { ApiTransportPort } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { SessionGate } from './session-gate.tsx';
import { useCurrentUser } from './current-user.tsx';
afterEach(cleanup);
function Current() {
  const value=useCurrentUser();
  return <output>{value === null ? 'unknown' : JSON.stringify(value)}</output>;
}
it('has no invented viewer before a verified auth owner supplies identity',()=>{
  render(<Current/>);expect(screen.getByRole('status').textContent).toBe('unknown');
});
it('projects actual whoami UID/name and clears old artifacts on a verified UID switch',async()=>{
  const client=new QueryClient();
  const clear=vi.spyOn(client,'clear');
  const transport=(userId:string):ApiTransportPort=>({send:()=>Promise.resolve({status:200,statusText:'OK',body:{
    userId,displayName:userId,role:'owner-looking-label',sessionId:'session-secret-not-for-business-context',
  }})});
  const unauthorized=createUnauthorizedChannel({enqueue:task=>task()});
  const runtime={deleteDatabase:vi.fn(),idbDatabaseName:'fixture'};
  const cursorStore={clear:vi.fn()};
  const tree=(value:ApiTransportPort)=><SessionGate transport={value} unauthorized={unauthorized} client={client}
    runtime={runtime} cursorStore={cursorStore} renderLogin={()=><p>login</p>} renderError={()=><p>failed</p>}><Current/></SessionGate>;
  const view=render(tree(transport('jian')));
  await waitFor(()=>expect(screen.getByRole('status').textContent).toBe('{"userId":"jian","displayName":"jian"}'));
  expect(screen.getByRole('status').textContent).not.toContain('session-secret');
  client.setQueryData(['harness-items','private','conversation_v1'],{old:'private body'});
  view.rerender(tree(transport('alice')));
  await waitFor(()=>expect(screen.getByRole('status').textContent).toBe('{"userId":"alice","displayName":"alice"}'));
  expect(clear).toHaveBeenCalledOnce();
  expect(client.getQueryData(['harness-items','private','conversation_v1'])).toBeUndefined();
});
