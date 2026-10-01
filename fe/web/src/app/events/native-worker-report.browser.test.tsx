import { QueryClient, QueryClientProvider, useQuery } from '@tanstack/react-query';
import { cleanup, render } from '@testing-library/react';
import { page } from 'vitest/browser';
import { afterEach, expect, it } from 'vitest';

import '../../styles/entry.css';
import type { ApiTransportPort } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { decodeEventFrame } from '../../../../core/events/protocol.ts';
import type { EventFrameHandler, UnconfiguredEventStream } from '../../systems/events/event-stream.ts';
import { createCardRegistry, registerAvailableBuiltinCards, type CardHostCapabilities } from '../../systems/cards/public.ts';
import { trackDetailQueryOptions } from '../providers/queries.ts';
import { EventBridge } from './event-bridge.tsx';

afterEach(cleanup);

it.each([
  { kind: 'task.completed', inFlight: false }, { kind: 'task.failed', inFlight: false },
  { kind: 'task.completed', inFlight: true }, { kind: 'task.failed', inFlight: true },
] as const)(
  'refreshes native-only card after $kind with initial read inFlight=$inFlight', async ({ kind, inFlight }) => {
    await page.viewport(390,844);
    const client = new QueryClient({ defaultOptions: { queries: { retry:false, staleTime:Infinity } } });
    const handlers = new Set<EventFrameHandler>();
    const stream: UnconfiguredEventStream = {
      on: () => () => {},
      onFrame: (handler) => { handlers.add(handler); return () => { handlers.delete(handler); }; },
      onConnectionState: () => () => {},
      configure: () => ({ start: () => {}, stop: () => {} }),
    };
    let reported = false;
    let reads = 0;
    const completed = kind === 'task.completed';
    const result = completed ? 'No blocking findings from the original report.' : { reason:'Original review failure.', details:{ check:'lease lifetime' } };
    let finishInitial: (() => void) | null = null;
    const transport: ApiTransportPort = { send: () => {
      reads += 1;
      const response = {status:200,statusText:'OK',body:{
        track:{ id:'track-a',area_id:'area-a',title:'Review',sort:0,cwd:'/repo',pinned_at:null,closed_at:null,created_at:1,updated_at:2 },
        can_reopen:false,can_close:true,overlays:[],cards:[{
          id:'reader',track_id:'track-a',kind:'codex',title:null,sort:0,deletable:true,created_at:1,updated_at:2,
          payload:{cwd:'/repo',worker_presentation:{kind:'native_only'},worker_snapshot:{
            task_id:'opaque-attempt',goal:'Review lifecycle safety',status:reported ? completed ? 'done':'failed':'running',
            report:reported ? {kind:'reported',outcome:completed?'completed':'failed',result}:{kind:'pending'},
          }},
        }],
      }};
      if (inFlight && reads === 1) return new Promise((resolve) => { finishInitial = () => resolve(response); });
      return Promise.resolve(response);
    }};
    const unauthorized = createUnauthorizedChannel({ enqueue:(work) => work() });
    const host = { lifecycle:{getSnapshot:() => ({visible:true,focused:false,geometry:{w:390,h:844},refresh:0}),subscribe:() => () => {}},
      slots:{use:() => [{current:null}]},emit:() => {},
    } as unknown as CardHostCapabilities;
    const registry = createCardRegistry();
    registerAvailableBuiltinCards(registry);
    const entry = registry.get('codex');
    if (entry === undefined) throw new Error('builtin native worker entry must exist');
    const Component = entry.component;
    function Worker() {
      const query = useQuery(trackDetailQueryOptions(transport,'track-a',unauthorized));
      const wire = query.data?.cards[0];
      const card = wire === undefined ? null : registry.resolve(wire);
      return card === null ? null : <Component card={card} host={host} activity={null}/>;
    }
    const view = render(<QueryClientProvider client={client}>
      <EventBridge client={client} stream={stream} syncEventVersion={1} dbInstanceId="db-native"
        cursor={{read:() => null,write:() => {},adopt:() => {},clear:() => {}}}/>
      <Worker/>
    </QueryClientProvider>);
    await expect.poll(() => reads).toBe(1);
    if (!inFlight) await expect.element(page.getByRole('status')).toHaveTextContent('Reviewing…');
    reported = true;
    const decoded = decodeEventFrame({_id:1,eventVersion:1,ev:kind,data:completed
      ? {idempotency_key:'opaque-attempt',result,artifacts:[]}
      : {idempotency_key:'opaque-attempt',reason:'Original review failure.',details:{check:'lease lifetime'}}});
    if (decoded.status !== 'ready') throw new Error('valid report frame must decode');
    for (const handler of handlers) handler(decoded.frame);
    if (inFlight) {
      const finish = finishInitial as (() => void) | null;
      if (finish === null) throw new Error('initial read must be pending');
      finish();
    }
    await expect.element(page.getByRole('status')).toHaveTextContent(completed?'Review finished':'Review failed');
    await expect.element(page.getByText(completed?'No blocking findings from the original report.':'Original review failure.',{exact:false})).toBeVisible();
    expect(reads).toBe(2);
    expect(document.querySelector('[data-nc-terminal-card]')).toBeNull();
    expect(document.querySelector('input,textarea,[contenteditable="true"]')).toBeNull();
    view.unmount();client.clear();
  },
);
