import { mailPerHour } from '../../../../core/domain/settings.ts';
import { SettingRow, SettingsList, SettingsPane } from './public.tsx';
import { SettingEditor, type SettingEditorCopy, type SettingEditorProps } from './setting-editor.tsx';

const COUNT_ERROR = 'Enter a whole number from 1 to 1000.';

function readMailCount(value: string | undefined) {
  const rate = mailPerHour(value);
  return rate.ok
    ? { ok: true as const, text: rate.text.slice(0, -2) }
    : { ok: false as const, error: COUNT_ERROR };
}

const MAIL_LIMIT: SettingEditorCopy = Object.freeze({
  label: 'Sending limit (messages per hour)',
  unit: '/h',
  inputMode: 'numeric',
  name: 'mail limit',
  resetLabel: 'Reset mail sending limit to default',
  parseStored(value: string | undefined) { return readMailCount(value); },
  parse(value: string | undefined) {
    return readMailCount(value === undefined ? undefined : `${value}/h`);
  },
});

export function MailPane({ value, onSave, ...props }: SettingEditorProps) {
  return <SettingsPane category="mail" title="Mail">
    <SettingsList>
      <SettingRow title="Sending limit" stackControlOnMobile={false}
        control={<SettingEditor {...props} copy={MAIL_LIMIT} inline
          value={value} onSave={(count) => onSave(count === null ? null : `${count}/h`)} />} />
    </SettingsList>
  </SettingsPane>;
}
