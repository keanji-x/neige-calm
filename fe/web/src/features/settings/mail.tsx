import { Heading as AstryxHeading } from '@astryxdesign/core/Heading';
import { Text as AstryxText } from '@astryxdesign/core/Text';

import { mailPerHour } from '../../../../core/domain/settings.ts';
import { SettingsPane } from './public.tsx';
import { SettingEditor, type SettingEditorCopy, type SettingEditorProps } from './setting-editor.tsx';
import styles from './settings.module.css';

const MAIL_LIMIT: SettingEditorCopy = Object.freeze({
  label: 'Messages per hour',
  unit: '',
  inputMode: 'text',
  name: 'mail limit',
  description: 'Use a rate like 5/h.',
  resetLabel: 'Restore default mail limit',
  parse(value: string | undefined) { return mailPerHour(value); },
});

export function MailPane(props: SettingEditorProps) {
  return <SettingsPane category="mail" title="Mail" lede="Manage how often Tracks can send mail.">
    <section className={styles.mailCard} aria-labelledby="nc-mail-limit-heading">
      <div className={styles.mailCardIntro}>
        <AstryxHeading level={4} id="nc-mail-limit-heading">Sending limit</AstryxHeading>
        <AstryxText as="p" color="secondary">
          Each Track shares this limit across outgoing messages and replies in any 60-minute window.
        </AstryxText>
      </div>
      <SettingEditor {...props} copy={MAIL_LIMIT} inlineActions />
      <p className={styles.mailHint}>Press Enter or leave the field to save. Changes apply immediately.</p>
    </section>
  </SettingsPane>;
}
