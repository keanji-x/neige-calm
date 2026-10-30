import { describe, expect, it } from 'vitest';
import { editQueuedMessageOperation, withdrawQueuedMessageOperation, steerQueuedMessageOperation, unlistedPendingInputCount, pendingQueueWithCurrentHumanVersions, pendingQueueOwnsHumanEntry } from './conversation-queue-controls.js';

describe('canonical queued message controls', () => {
  it('reconciles only listed own Human identities and preserves queue position', () => {
    const rows=[{entry_id:'message',rev:1,text:'old',queued_at_ms:123}];
    const source={id:'human:message',author:'you' as const,text:'new',atMs:9,
      human:{messageId:'message',revision:2,userId:'me',displayName:'Me'}};
    expect(pendingQueueWithCurrentHumanVersions(rows,[{...source,author:'human'}])).toBe(rows);
    const current=pendingQueueWithCurrentHumanVersions(rows,[source,{...source,id:'human:other',human:{...source.human,messageId:'other'}}]);
    expect(current).toEqual([{...rows[0],rev:2,text:'new'}]);
    const versions=new Map([['message',2]]);
    expect(pendingQueueOwnsHumanEntry({...source,human:{...source.human,revision:1}},versions)).toBe(true);
    expect(pendingQueueOwnsHumanEntry(source,versions)).toBe(true);
    expect(pendingQueueOwnsHumanEntry({...source,human:{...source.human,revision:3}},versions)).toBe(false);
  });
  it('binds edits, withdrawal and steering to their actual carrier, revision and native turn', () => {
    expect(editQueuedMessageOperation('card','carrier','message',3,'new text')).toMatchObject({
      method:'PATCH',path:'/api/cards/card/conversation/messages/message',body:{carrier_id:'carrier',if_revision:3,text:'new text'},
    });
    expect(withdrawQueuedMessageOperation('card','carrier','message',3)).toMatchObject({
      method:'DELETE',body:{carrier_id:'carrier',if_revision:3},
    });
    const steer=steerQueuedMessageOperation('card','carrier','message',3,'original-turn');
    expect(steer.body).toEqual({carrier_id:'carrier',if_revision:3,turn_id:'original-turn'});
    expect(steer.responseSchema.safeParse({submission_id:'submission',turn_id:'later-turn',delivery:'accepted'}).success).toBe(false);
    expect(steer.responseSchema.safeParse({submission_id:'submission',turn_id:'original-turn',delivery:'unknown'}).success).toBe(true);
  });
  it('counts Human and System pending input once by kind and exact revision', () => {
    const inputs=[{kind:'human' as const,message_id:'same',revision:2,queue_position:0,queued_at_ms:1},
      {kind:'system' as const,observation_id:'same',queue_position:1,queued_at_ms:2}];
    expect(unlistedPendingInputCount(inputs,[{entry_id:'same',rev:2,text:'queued',queued_at_ms:1}])).toBe(1);
    expect(unlistedPendingInputCount(inputs,[{entry_id:'same',rev:1,text:'old',queued_at_ms:1}])).toBe(2);
  });
});
