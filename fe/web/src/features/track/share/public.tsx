import { useRef } from 'react';
import { Button } from '@astryxdesign/core/Button';
import { Dialog, type DialogOrigin } from '../../../ui/dialog/public.tsx';
import styles from './share.module.css';

export type ShareDialogProps = Readonly<{
  origin: DialogOrigin; title: string; scopeKind: 'area' | 'track';
  url: string | null; onCopy: (text: string) => void; busy: boolean; error: string | null;
  onCreate: () => void; onClose: () => void;
}>;
/** The app owns generation, persistence and copying; this feature presents progress and the one-time result. */
export function ShareDialog({ origin, title, scopeKind, url, onCopy, busy, error, onCreate, onClose }: ShareDialogProps) {
  const primaryActionRef = useRef<HTMLButtonElement | null>(null);
  return <Dialog purpose="info" origin={origin} open onClose={onClose}
    title={scopeKind === 'area' ? '分享项目' : '分享链接'} initialFocusRef={primaryActionRef}>
    <div className={styles.content}>
      <p className={styles.description}>拿到链接的人可{scopeKind === 'area'
        ? `浏览「${title}」下的全部报告并在对话中提问。` : '阅读这份报告并在对话中提问。'}</p>
      {url !== null && <div className={styles.linkBox}>
        <input aria-label="分享链接" readOnly value={url}
          onClick={event => event.currentTarget.select()} onFocus={event => event.currentTarget.select()} />
      </div>}
      {url !== null && <p className={styles.hint}>口令仅展示一次，服务端不保存 · 24 小时内有效</p>}
      {busy && url === null && <p role="status">正在创建分享链接…</p>}
      {error !== null && <p role="alert">{error}</p>}
      <div className={styles.actions}>
        <Button variant="ghost" label="关闭" onClick={onClose} />
        {url === null
          ? error !== null && !busy && <Button ref={primaryActionRef} variant="primary" label="重试" onClick={onCreate} />
          : <Button ref={primaryActionRef} variant="primary" label="复制" onClick={() => onCopy(url)} />}
      </div>
    </div>
  </Dialog>;
}
