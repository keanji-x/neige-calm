import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import type { ApiRequest, ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { MobileAccessHost } from './mobile-access-host.tsx';

afterEach(cleanup);
function status(enabled: boolean) {
 return {provider:'private-tailnet',available:true,publicUrl:enabled?'https://fixture.example.ts.net':null,pending:[],devices:[],
 tailnet:{desiredEnabled:enabled,phase:enabled?'online':'disabled',nodeState:enabled?'online':'stopped',processRunning:enabled,childPid:enabled?123:null,
 httpsReady:enabled,upstreamReady:enabled,origin:enabled?'https://fixture.example.ts.net':null,dnsName:enabled?'fixture.example.ts.net':null,nodeId:enabled?'fixture':null,addresses:[],detail:enabled?'Ready':'Disabled'}};
}
const ok=(body:unknown):ApiTransportResponse=>({status:200,statusText:'OK',body});
function issued(){return {enrollmentId:'revoked',qrPayload:'neige-enroll:v2:fixture',qrImage:'data:image/svg+xml;base64,PHN2Zy8+',authKeyExpiresAt:Date.now()+300_000,pairExpiresAt:Date.now()+180_000};}
function mount(create:()=>Promise<ApiTransportResponse>){
 let current=status(true);
 const client=new QueryClient({defaultOptions:{queries:{retry:false}}});
 const send=vi.fn((request:ApiRequest)=>{
  if(request.path==='/api/mobile/access') return Promise.resolve(ok(current));
  if(request.method==='POST') return create();
  return Promise.resolve(ok({pendingCleanup:0,detail:'No pending cleanup'}));
 });
 render(<QueryClientProvider client={client}><MobileAccessHost transport={{send}} unauthorized={createUnauthorizedChannel({enqueue:(task)=>task()})} onBack={()=>undefined}/></QueryClientProvider>);
 return {send,change:async(next:ReturnType<typeof status>)=>{current=next;await act(async()=>{await client.refetchQueries({queryKey:['mobile-access']});});await screen.findByRole('button',{name:next.tailnet.desiredEnabled?'Disable':'Enable'});}};
}
it('retires a QR after observed external disable and never restores it on enable',async()=>{
 const view=mount(()=>Promise.resolve(ok(issued())));
 fireEvent.click(await screen.findByRole('button',{name:'Add phone'}));await screen.findByAltText('Scan once to join and pair this Neige workspace');
 await view.change(status(false));await view.change(status(true));
 expect(screen.queryByAltText('Scan once to join and pair this Neige workspace')).toBeNull();
 expect(view.send.mock.calls.filter(([r])=>r.method==='POST')).toHaveLength(1);
});
it('cancels delayed create after authoritative external disable',async()=>{
 let resolve:(r:ApiTransportResponse)=>void=()=>{throw new Error('uninitialized fixture');};
 const pending=new Promise<ApiTransportResponse>((done)=>{resolve=done;});const view=mount(()=>pending);
 fireEvent.click(await screen.findByRole('button',{name:'Add phone'}));await waitFor(()=>expect(view.send.mock.calls.some(([r])=>r.method==='POST')).toBe(true));
 await view.change(status(false));await act(async()=>{resolve(ok(issued()));await pending;});
 await waitFor(()=>expect(screen.getByRole('button',{name:'Enable'}).hasAttribute('disabled')).toBe(false));
 await view.change(status(true));expect(screen.queryByAltText('Scan once to join and pair this Neige workspace')).toBeNull();
 expect(view.send.mock.calls.filter(([r])=>r.method==='DELETE')).toHaveLength(1);
});

it('fences delayed create even after external disable has already been re-enabled',async()=>{
 let resolve:(r:ApiTransportResponse)=>void=()=>{throw new Error('uninitialized fixture');};
 const pending=new Promise<ApiTransportResponse>((done)=>{resolve=done;});const view=mount(()=>pending);
 fireEvent.click(await screen.findByRole('button',{name:'Add phone'}));await waitFor(()=>expect(view.send.mock.calls.some(([r])=>r.method==='POST')).toBe(true));
 await view.change(status(false));await view.change(status(true));
 await act(async()=>{resolve(ok(issued()));await pending;});
 await waitFor(()=>expect(screen.getByRole('button',{name:'Disable'}).hasAttribute('disabled')).toBe(false));
 expect(screen.queryByAltText('Scan once to join and pair this Neige workspace')).toBeNull();
 expect(view.send.mock.calls.filter(([r])=>r.method==='DELETE')).toHaveLength(1);
});

it.each(['origin','node'])('retires QR on authoritative %s change',async(kind)=>{
 const view=mount(()=>Promise.resolve(ok(issued())));
 fireEvent.click(await screen.findByRole('button',{name:'Add phone'}));await screen.findByAltText('Scan once to join and pair this Neige workspace');
 const next=status(true);
 if(kind==='origin'){next.tailnet.origin='https://other.example.ts.net';next.publicUrl=next.tailnet.origin;}else{next.tailnet.nodeId='replacement-node';}
 await view.change(next);expect(screen.queryByAltText('Scan once to join and pair this Neige workspace')).toBeNull();
});

it('keeps an issued QR through offline degraded status without inventing revocation',async()=>{
 const view=mount(()=>Promise.resolve(ok(issued())));
 fireEvent.click(await screen.findByRole('button',{name:'Add phone'}));await screen.findByAltText('Scan once to join and pair this Neige workspace');
 const next=status(true);next.tailnet.phase='degraded';next.tailnet.nodeState='offline';next.tailnet.httpsReady=false;next.tailnet.upstreamReady=false;next.tailnet.origin=null;next.tailnet.nodeId=null;
 await view.change(next);await view.change(status(true));
 expect(screen.getByAltText('Scan once to join and pair this Neige workspace')).toBeTruthy();
 expect(view.send.mock.calls.filter(([r])=>r.method==='DELETE')).toHaveLength(0);
});
