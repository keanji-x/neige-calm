import type { ReactNode } from 'react';
import { ConnectionNotice, LoginPage, type LoginPageProps } from '../../features/auth/login-page/public.tsx';
import { useState } from '../../ui/state/public.ts';

type NoticeKind = 'checking' | 'pairing' | 'unreachable' | 'app-update' | 'server-update';

function message(kind: NoticeKind): Readonly<{ title: string; detail: string }> {
  switch (kind) {
    case 'checking': return { title: '正在连接工作区', detail: '界面已就绪，正在确认服务器连接…' };
    case 'pairing': return { title: '扫码连接你的工作区', detail: '当前没有有效连接，请在电脑端设置中生成二维码，返回连接页扫码。' };
    case 'unreachable': return { title: '暂时无法连接服务器', detail: '请检查手机网络和电脑端远程连接是否开启，然后重试。' };
    case 'app-update': return { title: '请更新 Neige App', detail: '服务器已升级，请安装新版 App 后重新连接。刷新页面不会更新安装包中的界面。' };
    case 'server-update': return { title: '请更新电脑端 Neige', detail: '这台服务器版本较旧，请更新电脑端后重新连接。' };
  }
}

export function BundledConnectionNotice({ kind, children }: Readonly<{ kind: NoticeKind; children?: ReactNode }>) {
  return <ConnectionNotice {...message(kind)} busy={kind === 'checking'} reconnectHref="http://tauri.localhost/">
    {children}
  </ConnectionNotice>;
}

/** Manual login remains available for servers that also expose owner login. */
export function BundledLoginPage({ login, reload }: LoginPageProps) {
  const [manual, setManual] = useState(false);
  return manual ? <LoginPage login={login} reload={reload} onBackToPairing={() => { setManual(false); }} /> : <BundledConnectionNotice kind="pairing">
    <button type="button" onClick={() => { setManual(true); }}>使用账号登录</button>
  </BundledConnectionNotice>;
}
