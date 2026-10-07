import { PLANNER_CONVERSATION_KIND } from '../conversations/planner-row.ts';
import { mobileFontClassName } from '../../ui/mobile-font/public.ts';
import { useCallback, useRef } from 'react';
import { useState } from '../../ui/state/public.ts';
import type { Track } from '../../../../core/domain/track.ts';
import { MobileTrackList } from './mobile-track-list.tsx';
import type { SidebarTrackGroups } from './sidebar-track-groups.ts';
import { MobileNav } from '@astryxdesign/core/MobileNav';
import { Button } from '@astryxdesign/core/Button';
import { Icon } from '@astryxdesign/core/Icon';
import { DropdownMenu } from '@astryxdesign/core/DropdownMenu';
import type { Conversation } from '../../../../core/domain/conversation.ts';
import { conversationName } from '../../../../core/domain/conversation.ts';
import { Icon as UiIcon } from '../../ui/icon/public.tsx';
import { ErrorBox } from '../../ui/error-box/public.tsx';
import styles from './mobile-history.module.css';

type HistoryView = 'current' | 'pinned' | 'unread' | 'waiting';
const VIEWS: readonly HistoryView[] = Object.freeze(['current', 'pinned', 'unread', 'waiting']);
const VIEW_LABELS: Readonly<Record<HistoryView, string>> = Object.freeze({ current: '本 Track', pinned: '已置顶', unread: '未读', waiting: '未处理' });
const EMPTY_TRACK_LABELS = Object.freeze({ pinned: '还没有已置顶的 Track。', unread: '没有未读的 Track。', waiting: '没有需要处理的 Track。' });

function historyTitle(row: Conversation): string {
  const name = conversationName(row);
  return row.trackTitle && (row.kind === PLANNER_CONVERSATION_KIND || row.title === null) ? row.trackTitle : name;
}

function historySubtitle(row: Conversation): string {
  return historyTitle(row) === row.trackTitle ? conversationName(row) : row.trackTitle ?? '';
}

function groupName(at: number, now: number): string {
  const today = new Date(now); today.setHours(0, 0, 0, 0);
  const yesterday = new Date(today); yesterday.setDate(today.getDate() - 1);
  return at >= today.getTime() ? '今天' : at >= yesterday.getTime() ? '昨天' : '更早';
}

export function MobileHistory({ open, onOpenChange, conversations, loading, failed, onRetry, onNew, onSelect,
  onOpenSettings, accountLabel, accountInitial, onSignOut, now, scopeLabel, selectedConversationId, trackGroups, currentTrackId, onOpenTrack, isUnread, tracksLoading, tracksError, onRetryTracks }: Readonly<{
  selectedConversationId: string | null;
  trackGroups: Pick<SidebarTrackGroups, 'pinned' | 'unread' | 'waiting'>;
  currentTrackId: string | undefined;
  onOpenTrack: (trackId: string) => void;
  isUnread: (track: Track) => boolean;
  tracksLoading: boolean;
  tracksError: string | null;
  onRetryTracks: () => void;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  conversations: readonly Conversation[];
  loading: boolean;
  failed: boolean;
  onRetry: () => void;
  onNew: (() => void) | null;
  onSelect: (conversation: Conversation) => void;
  onOpenSettings: () => void;
  scopeLabel: string;
  accountLabel: string;
  accountInitial: string;
  onSignOut: () => void;
  now: number;
}>) {
  const [view, setView] = useState<HistoryView>('current');
  const wasOpen = useRef(open);
  if (wasOpen.current !== open) { wasOpen.current = open; if (open) setView('current'); }
  const list = useRef<HTMLDivElement | null>(null);
  const afterClose = useRef<(() => void) | null>(null);
  const dialog = useRef<HTMLDialogElement | null>(null);
  const didClose = useCallback(() => { const action = afterClose.current; afterClose.current = null; action?.(); }, []);
  const captureDialog = useCallback((node: HTMLDialogElement | null) => {
    dialog.current?.removeEventListener('close', didClose);
    dialog.current = node;
    node?.addEventListener('close', didClose);
  }, [didClose]);
  const leave = (action: () => void) => { afterClose.current = action; onOpenChange(false); };
  const groups = new Map<string, Conversation[]>();
  for (const row of conversations) {
    const name = groupName(row.updatedAt, now);
    const group = groups.get(name) ?? []; group.push(row); groups.set(name, group);
  }
  return <MobileNav id="mobile-conversation-history" ref={captureDialog} isOpen={open} onOpenChange={onOpenChange} side="start" width={320}
    label={view === 'current' ? '历史对话' : VIEW_LABELS[view]} header={<span className={styles.drawerHeading}>{view === 'current' ? '历史对话' : VIEW_LABELS[view]}</span>} className={`${styles.drawer} ${mobileFontClassName}`}
    >
    <div className={styles.layout}>
      <nav className={styles.views} aria-label="侧边栏视图">
        {VIEWS.map((candidate) => {
          const count = candidate === 'current' ? conversations.length : trackGroups[candidate].length;
          return <button key={candidate} type="button" className={styles.view} aria-label={VIEW_LABELS[candidate]}
            aria-pressed={view === candidate} onClick={() => { setView(candidate); if (list.current !== null) list.current.scrollTop = 0; }}>
            <span>{VIEW_LABELS[candidate]}</span>{count > 0 && <span className={styles.count} aria-hidden="true">{count > 99 ? '99+' : count}</span>}
          </button>;
        })}
      </nav>
      {view === 'current' && <div className={styles.functions}>
        <Button className={styles.functionButton} label="发起新对话" variant="ghost" size="lg" isDisabled={onNew === null}
          icon={<UiIcon name="plus" />} onClick={() => { if (onNew !== null) leave(onNew); }} />
        <div className={styles.searchWrap}><span className={styles.searchIcon}><Icon icon="search" color="inherit" /></span><input className={styles.search} type="search" name="history-search" aria-label="搜索对话内容（暂未开放）"
          placeholder="搜索对话内容（暂未开放）" disabled /></div>
      </div>}
      <div ref={list} className={styles.list}>
        {view === 'current' ? <>
        <p className={styles.scope}>{scopeLabel}</p>
        {failed && <ErrorBox message="部分历史对话未能加载。" actionLabel="重试" onRetry={onRetry} />}
        {loading && <p role="status">正在加载历史对话…</p>}
        {[...groups].map(([name, rows]) => <section key={name} aria-label={name}>
          <h2 className={styles.group}>{name}</h2>
          {rows.map((row) => <button key={row.id} type="button" className={styles.row} aria-current={row.id === selectedConversationId ? 'true' : undefined}
            aria-label={`${historyTitle(row)}${historySubtitle(row) ? ` ${historySubtitle(row)}` : ''}`} onClick={() => leave(() => onSelect(row))}>
            <span className={styles.title}>{historyTitle(row)}</span>
            <span className={styles.meta} title={historySubtitle(row)}>{row.kind === PLANNER_CONVERSATION_KIND ? null : historySubtitle(row)}<span aria-hidden="true">{row.kind === PLANNER_CONVERSATION_KIND ? '' : ' · '}{new Date(row.updatedAt).toLocaleDateString('zh-CN', { year: 'numeric', month: '2-digit', day: '2-digit' }).replaceAll('/', '-')}</span></span>
          </button>)}
        </section>)}
        {!loading && !failed && conversations.length === 0 && <p className={styles.empty}>当前 Track 还没有对话。</p>}
        </> : <>
          <p className={styles.scope}>全部工作区</p>
          {tracksError !== null && <ErrorBox message={tracksError} actionLabel="重试" onRetry={onRetryTracks} />}
          {tracksLoading && <p role="status">正在加载 Tracks…</p>}
          <MobileTrackList tracks={trackGroups[view]} currentTrackId={currentTrackId} isUnread={isUnread}
            onOpenTrack={(trackId) => leave(() => onOpenTrack(trackId))}
            emptyMessage={!tracksLoading && tracksError === null ? EMPTY_TRACK_LABELS[view] : undefined} />
        </>}
      </div>
      <div className={styles.footer}>
        <DropdownMenu placement="above" alignment="start" hasChevron={false}
          button={{ label: accountLabel, variant: 'ghost', size: 'lg', className: styles.footerButton, icon: <span className={styles.avatar}>{accountInitial}</span> }}
          items={[{ label: '退出登录', onClick: () => leave(onSignOut) }]} />
        <Button className={styles.footerButton} label="设置" variant="ghost" size="lg" icon={<Icon icon="wrench" />}
          onClick={() => leave(onOpenSettings)} />
      </div>
    </div>
  </MobileNav>;
}
