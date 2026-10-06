import { Tooltip } from '@astryxdesign/core/Tooltip';
import { Popover } from '@astryxdesign/core/Popover';
import { useState } from '../../ui/state/public.ts';
import { useConnectionStatus } from '../providers/connection-status.tsx';
import styles from './connection-indicator.module.css';

/** Connection health and workspace read failures share one quiet disclosure; each error is the read rule's sentence. */
export function ConnectionIndicator({ readError, activityError, loading, onRetry }: Readonly<{
  readError: string | null; activityError: string | null; loading: boolean; onRetry: () => void;
}>) {
  const connection = useConnectionStatus();
  const [open, setOpen] = useState(false);
  const errors = [readError, activityError]
    .filter((error): error is string => error !== null);
  const retryConnection = connection?.connected === false ? connection.retry : undefined;
  const retryRead = errors.length > 0;
  const healthy = connection?.connected === true && errors.length === 0;
  const label = errors.length > 0 ? (connection?.connected ? '已连接 · 数据读取失败' : '连接异常')
    : connection?.label ?? '连接状态未知';
  const details = [label, connection?.detail, loading ? '正在读取工作区…' : null, ...errors]
    .filter((detail): detail is string => typeof detail === 'string' && detail.length > 0);
  return <div className={styles.wrap}><Popover label="连接详情" isOpen={open} onOpenChange={setOpen}
    width="min(20rem, calc(100vw - 2rem))" placement="below" alignment="start"
    content={<div className={styles.details}>
      {details.map(detail => <p className={styles.line} key={detail}>{detail}</p>)}
      {(retryConnection !== undefined || retryRead) && <button type="button" className={styles.retry} onClick={() => {
        setOpen(false); retryConnection?.(); if (retryRead) onRetry();
      }}>{retryConnection === undefined ? '重试读取' : '立即重试'}</button>}
    </div>}>
    {props => <Tooltip content={<div>{details.map(detail => <div key={detail}>{detail}</div>)}</div>} placement="below" alignment="start">
      <button {...props} type="button" className={styles.trigger} aria-label={`连接状态：${label}`}
        data-nc-connection-status={healthy ? 'connected' : 'disconnected'}>
        <span className={styles.dot} aria-hidden="true" />
      </button>
    </Tooltip>}
  </Popover></div>;
}
