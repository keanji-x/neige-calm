import { useState } from '../../../ui/state/public.ts';
import { createCardHost } from '../host.ts';
import { createCardRegistry, type CardComponentProps, type CardEntry } from '../registry.ts';
import { BoardHost, type BoardHostItem } from './board-host.tsx';
import { CardHead } from './card-head.tsx';

type PreviewCard = Readonly<{ type: 'motion-preview'; id: string; title: string; detail: string }>;
declare module '../registry.js' { interface CardDataMap { motionPreview: PreviewCard } }

function PreviewCardView({ card, onRemove }: CardComponentProps<PreviewCard>) {
  return <div className="term">
    <CardHead className="card-drag-handle" title={card.title} onClose={onRemove} closeAriaLabel={`删除${card.title}`} />
    <div className="term-body"><p>{card.detail}</p></div>
  </div>;
}
const entry: CardEntry<PreviewCard> = Object.freeze({
  type: 'motion-preview', component: (props: CardComponentProps<PreviewCard>) => <PreviewCardView {...props} />,
  defaultSize: Object.freeze({ w: 12, h: 4, minW: 4, minH: 3 }),
  title: (card: PreviewCard) => card.title, accessibleName: (card: PreviewCard) => card.title,
  create: Object.freeze({ mode: 'kernel-minted-only' }),
});
function initialItems(): readonly BoardHostItem[] { return Object.freeze([
  { id: 'preview-arrange', title: '整理想法', detail: '删除这张卡片，看下面的卡片自然补位。' },
  { id: 'preview-working', title: '继续推进', detail: '移动途中拖住顶部，动作会立即交给你。' },
  { id: 'preview-result', title: '查看结果', detail: '拖动右下角调整尺寸，也可以在补位途中试试。' },
].map((card, originalIndex): BoardHostItem => Object.freeze({
  card: Object.freeze({ ...card, type: 'motion-preview' as const }), title: card.title, originalIndex, activity: null, notice: null,
}))); }

/** Development composition only: actual grid/headers, local cards, no kernel mapping or writes. */
export function CardMotionPreview() {
  const [host] = useState(() => { const registry = createCardRegistry(); registry.register(entry); return createCardHost(registry); });
  const [items, setItems] = useState(initialItems);
  const [generation, setGeneration] = useState(0);
  return <section aria-label="卡片布局预览" style={{ marginTop: 28 }}>
    <h2 style={{ fontSize: 22, marginBottom: 16 }}>卡片重排</h2>
    <p>删除第一张卡片，再试试补位途中拖拽顶部或拉动右下角。点击重置可以重新体验。</p>
    <button type="button" onClick={() => { setItems(initialItems()); setGeneration(value => value + 1); }}>重置卡片</button>
    <div style={{ display: 'flex', blockSize: 680, marginTop: 16 }}>
      <BoardHost key={generation} host={host} items={items} activeCardId={null} visible
        onRemoveCard={id => { setItems(current => current.filter(item => item.card.id !== id)); }} />
    </div>
    <p role="status">{items.length === 0 ? '卡片已全部移除，点击重置重新体验。' : `当前有 ${items.length} 张卡片。`}</p>
  </section>;
}
