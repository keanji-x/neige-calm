import { CheckboxInput } from '@astryxdesign/core/CheckboxInput';
import { TextInput } from '@astryxdesign/core/TextInput';
import { templateFieldValue, type TemplateInputForm, type TemplateInputValues } from '../../../../../core/domain/template-input.ts';
import styles from './template-inputs.module.css';

/** The template supplies content and grouping; this view owns only control rendering. */
export function TemplateInputs({ form, values, errors, disabled, onChange }: Readonly<{
  form: TemplateInputForm; values: TemplateInputValues; errors: Readonly<Record<string, string>>;
  disabled: boolean; onChange: (key: string, value: string) => void;
}>) {
  return <div className={styles.groups}>{form.groups.map((group, index) => <fieldset key={index} className={styles.group} aria-label={group.fields.length === 1 ? group.title : undefined}>
    {group.fields.length > 1 && <legend>{group.title}</legend>}
    {group.fields.length > 1 && group.description !== '' && <p className={styles.description}>{group.description}</p>}
    {group.fields.map((field) => {
      const value = templateFieldValue(field, values);
      const error = errors[field.key];
      const description = field.kind === 'text' ? field.help : value === field.on_value ? field.on_description : field.off_description;
      const help = description || (group.fields.length === 1 ? group.description : '');
      return field.kind === 'text' ? <TextInput key={field.key} label={field.label}
        value={value} placeholder={field.placeholder} width="100%" isDisabled={disabled}
        description={value.trim() !== '' && error !== undefined ? undefined : help || undefined}
        status={value.trim() !== '' && error !== undefined ? { type: 'error', message: error } : undefined}
        onChange={(next) => onChange(field.key, next)} />
      : <div key={field.key} className={styles.toggle}>
        <CheckboxInput label={field.label} value={value === field.on_value} isDisabled={disabled}
          description={help || undefined}
          onChange={(checked) => onChange(field.key, checked ? field.on_value : field.off_value)} />
        {error !== undefined && <p role="alert" className={styles.error}>{error}</p>}
      </div>;
    })}
  </fieldset>)}</div>;
}
