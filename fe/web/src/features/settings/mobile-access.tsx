import { Button as AstryxButton } from '@astryxdesign/core/Button';
import { Text as AstryxText } from '@astryxdesign/core/Text';
import type { MobileAccessStatus, MobileInvitation, TailnetLoginRequest } from '../../../../core/api/mobile-access.ts';
import { ErrorBox } from '../../ui/error-box/public.tsx';
import { SettingsPane, SettingsList, SettingRow } from './public.tsx';
import styles from './mobile-access.module.css';

export type MobileAccessPaneProps = Readonly<{
  status: MobileAccessStatus | undefined;
  invitation: MobileInvitation | null;
  login: TailnetLoginRequest | null;
  onLogin: () => void;
  onLogout: () => void;
  busy: boolean;
  error: string | null;
  onBack: () => void;
  onRefresh: () => void;
  onEnable: () => void;
  onDisable: () => void;
  onCreate: () => void;
  onApprove: (id: string) => void;
  onRevoke: (id: string) => void;
}>;

export function MobileAccessPane(props: MobileAccessPaneProps) {
  const { status, invitation, busy, error } = props;
  const tailnet = status?.tailnet;
  const enabled = tailnet?.desiredEnabled ?? status?.publicUrl !== null;
  return <>
    <AstryxButton variant="ghost" label="‹ Network" onClick={props.onBack} />
    <SettingsPane title="Mobile connection" lede="Connect through your private Tailnet, then approve the phone here to access this workspace.">
      {error !== null && <ErrorBox message={error} onRetry={props.onRefresh} />}
      {status === undefined && error === null && <AstryxText as="p">Loading connection status…</AstryxText>}
      {status !== undefined && <SettingsList>
        <SettingRow title="Remote access" description={status.available
          ? status.provider === 'private-tailnet' ? 'Private access through the node bundled with Neige. Disabling keeps its identity for next time.' : 'Uses the explicitly configured public Funnel tunnel.'
          : 'Mobile connection must first be configured on this server.'}
          control={status.available
            ? <AstryxButton isDisabled={busy} onClick={enabled ? props.onDisable : props.onEnable} label={enabled ? 'Disable' : 'Enable'} />
            : <AstryxText>Unavailable</AstryxText>} />
        {tailnet !== null && tailnet !== undefined && <>
          <SettingRow title="Tailnet node" description={tailnet.detail} control={<AstryxText>{tailnet.phase.replaceAll('-', ' ')}</AstryxText>} />
          {tailnet.desiredEnabled && <>
            <SettingRow title="HTTPS" description="MagicDNS and HTTPS certificates must be enabled in your Tailscale DNS settings."
              control={<AstryxText>{tailnet.httpsReady ? 'Ready' : 'Not ready'}</AstryxText>} />
            <SettingRow title="Neige connection" control={<AstryxText>{tailnet.upstreamReady ? 'Ready' : 'Waiting for Neige'}</AstryxText>} />
            {tailnet.origin !== null && <SettingRow title="Private address" control={<AstryxText>{tailnet.origin}</AstryxText>} />}
            {tailnet.nodeState === 'needs-login' && <SettingRow title="Sign in to Tailscale"
              description="Sign in here once to authorize this computer’s private node."
              control={props.login === null
                ? <AstryxButton isDisabled={busy} onClick={props.onLogin} label="Start sign-in" />
                : <a href={props.login.loginUrl} target="_blank" rel="noreferrer">Continue to Tailscale</a>} />}
            <SettingRow title="Sign out of Tailnet" description="Disables remote access and removes this computer’s private node identity."
              control={<AstryxButton isDisabled={busy || !tailnet.processRunning} onClick={props.onLogout} label="Sign out of Tailnet" />} />
          </>}
        </>}
        {status.publicUrl !== null && <SettingRow title="Pair a phone"
          description="Open Scan QR code in the Neige app, then compare the confirmation code here."
          control={<AstryxButton isDisabled={busy} onClick={props.onCreate} label="Create QR code" />} />}
      </SettingsList>}
      {status !== undefined && status.publicUrl !== null && invitation !== null && <div className={styles.invitation}>
        <img src={invitation.qrImage} alt="Scan to pair this Neige workspace" className={styles.qr} />
        <AstryxText as="p" color="secondary">This invitation expires in {invitation.expiresInSeconds} seconds and can be scanned once.</AstryxText>
      </div>}
      {status !== undefined && status.pending.length > 0 && <SettingsList>
        {status.pending.map((pair) => <SettingRow key={pair.id} title={pair.deviceName}
          description={`Approve only if your phone shows ${pair.verificationCode}.`}
          control={<AstryxButton isDisabled={busy} onClick={() => props.onApprove(pair.id)} label={`Approve ${pair.verificationCode}`} />} />)}
      </SettingsList>}
      {status !== undefined && status.devices.length > 0 && <SettingsList>
        {status.devices.map((device) => <SettingRow key={device.id} title={device.deviceName}
          description="Connected. Server restarts require a new scan."
          control={<AstryxButton isDisabled={busy} onClick={() => props.onRevoke(device.id)} label="Revoke" />} />)}
      </SettingsList>}
    </SettingsPane>
  </>;
}
