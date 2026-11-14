import { useEffect, useRef } from 'react';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { Button } from '@astryxdesign/core/Button';
import type { ApiTransportPort } from '../../../../core/api/types.ts';
import type { UnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import type { ShareLink, ShareScope, ShareLinkCreated } from '../../../../core/api/generated/wire.ts';
import { createShareLinkOperation, revokeShareLinkOperation, shareLinksOperation,
  sharePagePath } from '../../../../core/domain/share.ts';
import { ShareDialog } from '../../features/track/share/public.tsx';
import type { DialogOrigin } from '../../ui/dialog/public.tsx';
import { ContextGroup, ContextList } from '../../ui/panel-card/context.tsx';
import { Icon } from '../../ui/icon/public.tsx';
import { useState } from '../../ui/state/public.ts';
import { writeClipboardText } from '../../ui/operation-feedback/clipboard.ts';
import { runOperation } from './queries.ts';
import styles from './share-management.module.css';

export type ShareSelection = Readonly<{ scope: ShareScope; title: string; origin: DialogOrigin }>;
/** Clipboard API requires HTTPS; keep creation usable on LAN HTTP too. */
export async function copyShareUrl(url: string): Promise<boolean> {
  try { await writeClipboardText(url); return true; }
  catch {
    const focused = document.activeElement;
    const field = document.createElement('textarea');
    field.value = url; field.style.position = 'fixed'; field.style.left = '-9999px';
    try {
      document.body.append(field); field.focus(); field.select();
      return document.execCommand('copy');
    } catch { return false; }
    finally { field.remove(); if (focused instanceof HTMLElement) focused.focus(); }
  }
}
function shareUrl(id: string): string {
  return `${window.location.origin}${sharePagePath(id)}`;
}
function shareKey(scope: ShareScope) { return ['share-links', scope.kind, scope.id] as const; }
export function useShareManagementDialog(transport: ApiTransportPort, unauthorized: UnauthorizedChannel) {
  const [opened, setOpened] = useState<Readonly<{ selection: ShareSelection; request: Promise<ShareLinkCreated> }> | null>(null);
  const current = useRef<ShareSelection | null>(null);
  const queryClient = useQueryClient();
  const create = async (selection: ShareSelection) => {
    const result = await runOperation(transport, createShareLinkOperation(selection.scope), unauthorized);
    await queryClient.cancelQueries({ queryKey: shareKey(selection.scope) });
    await queryClient.invalidateQueries({ queryKey: shareKey(selection.scope) });
    return result;
  };
  return { openShare: (selection: ShareSelection) => {
    if (current.current?.scope.kind === selection.scope.kind && current.current.scope.id === selection.scope.id) return;
    current.current = selection;
    // Mint only from the explicit opener event, never from render or an effect.
    setOpened({ selection, request: create(selection) });
  }, shareDialog: opened === null ? null : <ShareManagement
    key={`${opened.selection.scope.kind}:${opened.selection.scope.id}`} selection={opened.selection} initialRequest={opened.request}
    create={() => create(opened.selection)} onClose={() => { current.current = null; setOpened(null); }} /> };
}
function ShareManagement({ selection, initialRequest, create, onClose }: {
  selection: ShareSelection; initialRequest: Promise<ShareLinkCreated>; create: () => Promise<ShareLinkCreated>; onClose: () => void;
}) {
  const [created, setCreated] = useState<ShareLinkCreated | null>(null);
  const [request, setRequest] = useState(initialRequest);
  const [busy, setBusy] = useState(true);
  const pending = useRef(true);
  const alive = useRef(true);
  useEffect(() => { alive.current = true; return () => { alive.current = false; }; }, []);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    let active = true;
    void request.then(result => { if (active) setCreated(result); }).catch(() => { if (active) setError('操作未完成，请重试。'); })
      .finally(() => { if (active) { pending.current = false; setBusy(false); } });
    return () => { active = false; };
  }, [request]);
  return <ShareDialog {...selection} scopeKind={selection.scope.kind} url={created === null ? null : `${shareUrl(created.link.delegation.id)}#${encodeURIComponent(created.token)}`}
    onCopy={text => { void copyShareUrl(text).then(ok => { if (!alive.current) return; if (ok) onClose(); else setError('无法复制，请手动复制。'); }); }} busy={busy} error={error}
    onClose={onClose} onCreate={() => {
      if (pending.current || created !== null) return;
      pending.current = true; setBusy(true); setError(null);
      setRequest(create());
    }} />;
}

/** Owner-only app composition. Both scopes list only non-secret link metadata. */
export function TrackShareLinks({ trackId, areaId, shareable, transport, unauthorized }: {
  trackId: string; areaId: string; shareable: boolean; transport: ApiTransportPort; unauthorized: UnauthorizedChannel;
}) {
  const trackScope: ShareScope = { kind: 'track', id: trackId };
  const areaScope: ShareScope = { kind: 'area', id: areaId };
  const track = useQuery({ queryKey: shareKey(trackScope), enabled: shareable,
    queryFn: ({ signal }) => runOperation(transport, { ...shareLinksOperation(trackScope), signal }, unauthorized) });
  const area = useQuery({ queryKey: shareKey(areaScope), enabled: shareable,
    queryFn: ({ signal }) => runOperation(transport, { ...shareLinksOperation(areaScope), signal }, unauthorized) });
  const [now, setNow] = useState(Date.now);
  useEffect(() => { const timer = setInterval(() => setNow(Date.now()), 30_000); return () => clearInterval(timer); }, []);
  const items = shareable ? [...(track.data ?? []), ...(area.data ?? [])].filter(link => link.revokedAt === null && link.expiresAt > now) : [];
  if (items.length === 0) return null;
  return <ContextGroup title="分享链接" count={items.length}>
    <ContextList aria-label="分享链接">{items.map(link => <ShareLinkRow key={link.delegation.id}
      link={link} now={now} transport={transport} unauthorized={unauthorized} />)}</ContextList>
  </ContextGroup>;
}
function ShareLinkRow({ link, now, transport, unauthorized }: {
  link: ShareLink; now: number; transport: ApiTransportPort; unauthorized: UnauthorizedChannel;
}) {
  const client = useQueryClient();
  const [copied, setCopied] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const label = link.delegation.scope.kind === 'track' ? '报告链接' : '项目链接';
  return <li>
    <div className={styles.row}>
      <Icon name="link" size="sm" />
      <span className={styles.label}>{label} · 剩余 {Math.ceil((link.expiresAt - now) / 3_600_000)} 小时</span>
      <Button className={styles.action} icon={<Icon name={copied ? 'check' : 'copy'} size="sm" />} isIconOnly label={copied ? '已复制' : '复制分享链接'} tooltip={copied ? '已复制' : '复制分享链接'} size="sm" variant="ghost" isDisabled={busy}
        onClick={() => { void copyShareUrl(shareUrl(link.delegation.id)).then(ok => { setCopied(ok); setError(ok ? null : '无法复制分享链接。'); }); }} />
      <Button className={styles.action} label="删除分享链接" icon={<Icon name="delete" size="sm" />} isIconOnly size="sm" variant="ghost" isDisabled={busy}
        onClick={() => {
          setBusy(true); setError(null);
          void runOperation(transport, revokeShareLinkOperation(link.delegation.id), unauthorized)
            .then(() => client.invalidateQueries({ queryKey: shareKey(link.delegation.scope) }))
            .catch(() => setError('无法删除分享链接。')).finally(() => setBusy(false));
        }} />
    </div>
    {error !== null && <div className={styles.error} role="alert">{error}</div>}
  </li>;
}
