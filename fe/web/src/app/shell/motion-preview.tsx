import { useRef } from 'react';
import { createRoot } from 'react-dom/client';
import { useState } from '../../ui/state/public.ts';
import { ReportDocument } from '../../features/report/document/public.tsx';
import { InventoryGroups } from '../../features/track/page/inventory-groups.tsx';
import { ContextRing } from '../../features/chat/thread/context-ring.tsx';
import { ActivityIndicator } from '../../ui/activity-indicator/public.tsx';
import { NeigeMotion } from '../../ui/brand/motion.tsx';
import { Dialog } from '../../ui/dialog/public.tsx';
import { Drawer } from '../../ui/drawer/public.tsx';
import { ChatComposer, ChatThread } from '../../features/chat/thread/public.tsx';
import { isComposerEmpty, isSameComposer } from '../../../../core/domain/conversation-composer.ts';
import type { Conversation, TranscriptEntry } from '../../../../core/domain/conversation.ts';

const shortText = '帮我看看 edit 的动画，让进入编辑更自然一点。';
const longText = '我们希望编辑消息时，原来的对话保持稳定，输入框平稳接住内容。文字应一直清晰可读，编辑条和输入区域一起展开，取消时清空未修改的内容，并保留你已修改的草稿。\n\n长消息也不需要从屏幕顶部飞到底部；可以通过局部的过渡，让人知道正在编辑哪条消息。动画期间仍然可以输入、取消或重新开始编辑。';
const conversation: Conversation = Object.freeze({ id: 'preview', trackId: 'preview', title: '动画预览', kind: 'codex', state: 'idle', updatedAt: 1 });
function Preview() {
  const [open, setOpen] = useState(true);
  const [percent, setPercent] = useState(10);
  const [dialogOpen, setDialogOpen] = useState(false);
  const dialogInput = useRef<HTMLInputElement>(null);
  const [prompt, setPrompt] = useState(shortText);
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState('');
  const [notice, setNotice] = useState('');
  const [focus, setFocus] = useState(0);
  const cancel = () => {
    setEditing(false);
    if (isSameComposer({ text: draft, attachments: [] }, { text: prompt, attachments: [] })) setDraft('');
  };
  const sample = (text: string) => { setPrompt(text); setEditing(false); setDraft(''); setNotice(''); setOpen(true); };
  const turns: TranscriptEntry[] = [
    { id: 'you', author: 'you', text: prompt, atMs: 1 },
    { id: 'reply', author: 'agent', text: '可以。保留消息的位置，让输入区平稳展开。你可以点下方的编辑按钮，试试输入、取消，再快速重新编辑。', atMs: 2 },
    { id: 'outcome', author: 'turn', turnId: 'turn', status: 'completed', elapsedMs: 840, atMs: 3 },
  ];
  return <main style={{ maxWidth: 1160, margin: '0 auto', padding: '40px 24px', color: 'var(--text)' }}>
    <div style={{ display: 'flex', flexWrap: 'wrap', gap: 24, alignItems: 'flex-end', justifyContent: 'space-between', marginBottom: 24 }}>
      <div><p style={{ color: 'var(--text-3)', fontSize: 12, letterSpacing: '.12em', marginBottom: 12 }}>NEIGE · 交互预览</p>
        <h1 style={{ fontSize: 30, fontWeight: 500, margin: '0 0 12px' }}>让编辑平稳接上</h1>
        <p style={{ color: 'var(--text-2)', margin: 0 }}>原消息留在原位，输入区轻柔展开，随时可以继续输入。</p></div>
      <div style={{ display: 'flex', gap: 8 }}>
        <button onClick={() => sample(shortText)}>短消息</button>
        <button onClick={() => sample(longText)}>长消息</button>
        <button onClick={() => setDialogOpen(true)}>打开弹窗</button>
        <button onClick={() => setOpen(!open)}>{open ? '收起对话' : '打开对话'}</button>
      </div>
    </div>
    <section style={{ position: 'relative', containerType: 'inline-size', height: 630, border: '1px solid var(--hairline)', borderRadius: 20, background: 'var(--surface-rail)' }}>
      <div style={{ padding: 32, maxWidth: '48%' }}>
        <p style={{ fontSize: 12, color: 'var(--text-3)', marginBottom: 24 }}>预览体验</p>
        <h2 style={{ fontWeight: 500, fontSize: 22, marginBottom: 20 }}>内容一直在，动作自然发生</h2>
        <p style={{ color: 'var(--text-2)', lineHeight: 1.8 }}>点对话里的铅笔开始编辑。试试长消息，或者展开途中立即取消，再重新编辑。</p>
        <p style={{ color: 'var(--text-3)', lineHeight: 1.8 }}>收起、打开对话，也可以感受新的进出节奏。</p>
        <button onClick={() => setOpen(true)}>打开对话</button>
        <p role="status" style={{ marginTop: 24, color: 'var(--text-2)' }}>{notice}</p>
      </div>
      <Drawer open={open} title="动画预览" onClose={() => setOpen(false)} footer={
        <ChatComposer draft={{ text: draft, onChange: setDraft }} focusRequest={focus}
          editing={editing ? { preview: prompt, onCancel: cancel } : undefined}
          onSend={(text: string) => {
            if (editing) { setPrompt(text); setEditing(false); setDraft(''); setNotice('预览消息已更新。'); }
            else { setDraft(''); setNotice('这条消息仅在预览中展示，不会发送。'); }
            return false;
          }} />
      }>
        <ChatThread conversation={conversation} turns={turns} cards={{}} stalled={false} canContinue={false}
          editing={editing ? 'outcome' : null}
          editMessage={!editing && isComposerEmpty({ text: draft, attachments: [] }) ? () => {
            setDraft(prompt); setEditing(true); setFocus(value => value + 1);
          } : undefined} />
      </Drawer>
    </section>
    <p style={{ color: 'var(--text-3)', fontSize: 12, marginTop: 16 }}>交互预览 · 使用实际对话组件 · 内容仅保存在当前页面</p>
    <section style={{ marginTop: 28 }}>
      <h2 style={{ fontSize: 22, marginBottom: 16 }}>折叠与展开</h2>
      <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fit, minmax(260px, 1fr))', gap: 24 }}>
        <ReportDocument report={{ summary: '', body: '', blocks: [
          { id: 'preview-prose', kind: 'prose', payload: { markdown: '# 组件动效\n\n展开内容保持即时，箭头轻柔接上。' } },
          { id: 'preview-task', kind: 'task', payload: { key: '检查动效', kind: 'codex', declared_by: 'user', ready: true, goal: '保留内容与键盘交互。' } },
        ] }} empty={<p>暂无内容</p>} />
        <InventoryGroups noun="task" groups={[{ key: 'working', label: '任务分组', expanded: true, rows: ['清晰可读', '操作自然'] }]}
          renderRows={rows => rows.map(row => <p key={row}>{row}</p>)} />
      </div>
    </section>
    <section style={{ marginTop: 28 }}>
      <h2 style={{ fontSize: 22, marginBottom: 16 }}>进度与状态</h2>
      <div style={{ display: 'flex', gap: 24, alignItems: 'center', flexWrap: 'wrap' }}>
        <button onClick={() => setPercent(value => value === 10 ? 65 : 10)}>更新进度</button>
        <ContextRing usage={{ used_tokens: percent * 100, context_window: 10000, percent, at_ms: 0 }} />
        <ActivityIndicator state="working" />
        {(['thinking', 'execution', 'creation'] as const).map(kind => <div key={kind} style={{ width: 48, height: 48 }}><NeigeMotion kind={kind} /></div>)}
      </div>
    </section>
    <Dialog open={dialogOpen} title="创建任务" initialFocusRef={dialogInput} onClose={() => setDialogOpen(false)}>
      <label>任务名称<input ref={dialogInput} defaultValue="整理今天的工作" /></label>
      <p>内容保持清晰，打开时轻柔接入，关闭后回到原来的位置。</p>
      <button data-nc-action="secondary" onClick={() => setDialogOpen(false)}>完成</button>
    </Dialog>
  </main>;
}
function mountPreview() {
  const root = createRoot(document.getElementById('root')!);
  root.render(<Preview />);
  import.meta.hot?.dispose(() => root.unmount());
}
mountPreview();
