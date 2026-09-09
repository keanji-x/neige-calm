import { Button as AstryxButton } from '@astryxdesign/core/Button';
import { Text as AstryxText } from '@astryxdesign/core/Text';
import type { MobileAccessStatus, MobileInvitation } from '../../../../core/api/mobile-access.ts';
import { ErrorBox } from '../../ui/error-box/public.tsx';
import { SettingsPane, SettingsList, SettingRow } from './public.tsx';
import styles from './mobile-access.module.css';

export type MobileAccessPaneProps = Readonly<{
  status: MobileAccessStatus | undefined;
  invitation: MobileInvitation | null;
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
  return <>
    <AstryxButton variant="ghost" label="‹ Network" onClick={props.onBack} />
    <SettingsPane title="Mobile connection" lede="Scan with Neige on your phone to connect from any network. No phone VPN is needed.">
      {error !== null && <ErrorBox message={error} onRetry={props.onRefresh} />}
      {status === undefined && error === null && <AstryxText as="p">Loading connection status…</AstryxText>}
      {status !== undefined && <SettingsList>
        <SettingRow title="Remote access" description={status.available
          ? 'Uses an encrypted tunnel. Your server does not need a public IP.'
          : 'Mobile connection must first be configured on this server.'}
          control={status.available
            ? <AstryxButton isDisabled={busy} onClick={status.publicUrl === null ? props.onEnable : props.onDisable} label={status.publicUrl === null ? 'Enable' : 'Disable'} />
            : <AstryxText>Unavailable</AstryxText>} />
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
