// Minimal schema-driven form used by the AddPanel "config card" flow.
//
// Intentionally not a generic JSON-Schema renderer — the only schemas
// rendered today are bundled with built-in card entries (`terminal`,
// `codex`). It supports just the field types those entries need
// (string / textarea / enum). Plugin-driven richer schemas come later,
// possibly through a different renderer entirely; we'd rather keep this
// file small than pre-build a generic that grows scope creep.

import { useState } from '../state';
import type { CreateField, CreateSchema } from '../../cards/registry';
import { DirectoryPicker } from './DirectoryPicker';

export type SchemaFormValues = Record<string, string>;

export interface SchemaFormProps {
  schema: CreateSchema;
  submitLabel?: string;
  onSubmit: (values: SchemaFormValues) => void | Promise<void>;
  onCancel: () => void;
}

function defaultsFor(schema: CreateSchema): SchemaFormValues {
  const v: SchemaFormValues = {};
  for (const f of schema.fields) {
    v[f.key] = f.default ?? '';
  }
  return v;
}

export function SchemaForm({
  schema,
  submitLabel = 'Create',
  onSubmit,
  onCancel,
}: SchemaFormProps) {
  const [values, setValues] = useState<SchemaFormValues>(() => defaultsFor(schema));
  const [submitting, setSubmitting] = useState(false);

  const missingRequired = schema.fields.some(
    (f) => f.required && !(values[f.key] ?? '').trim(),
  );

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (missingRequired || submitting) return;
    setSubmitting(true);
    try {
      await onSubmit(values);
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <form className="schema-form" onSubmit={handleSubmit}>
      {schema.fields.map((field) => (
        <FieldRow
          key={field.key}
          field={field}
          value={values[field.key] ?? ''}
          onChange={(v) => setValues((cur) => ({ ...cur, [field.key]: v }))}
        />
      ))}
      <div className="schema-form-actions">
        <button type="button" className="schema-form-cancel" onClick={onCancel}>
          Cancel
        </button>
        <button
          type="submit"
          className="schema-form-submit"
          disabled={missingRequired || submitting}
        >
          {submitting ? 'Creating…' : submitLabel}
        </button>
      </div>
    </form>
  );
}

function FieldRow({
  field,
  value,
  onChange,
}: {
  field: CreateField;
  value: string;
  onChange: (v: string) => void;
}) {
  const id = `sf-${field.key}`;
  return (
    <label htmlFor={id} className="schema-form-field">
      <span className="schema-form-label">
        {field.label}
        {field.required && <span className="schema-form-required"> *</span>}
      </span>
      {field.type === 'textarea' ? (
        <textarea
          id={id}
          className="schema-form-input"
          rows={4}
          value={value}
          placeholder={field.placeholder}
          onChange={(e) => onChange(e.target.value)}
        />
      ) : field.type === 'enum' ? (
        <select
          id={id}
          className="schema-form-input"
          value={value}
          onChange={(e) => onChange(e.target.value)}
        >
          {(field.options ?? []).map((opt) => (
            <option key={opt} value={opt}>
              {opt}
            </option>
          ))}
        </select>
      ) : field.type === 'directory' || field.type === 'file' ? (
        <DirectoryPicker
          id={id}
          value={value}
          onChange={onChange}
          placeholder={field.placeholder}
          mode={field.type === 'file' ? 'file' : 'directory'}
        />
      ) : (
        <input
          id={id}
          type="text"
          className="schema-form-input"
          value={value}
          placeholder={field.placeholder}
          onChange={(e) => onChange(e.target.value)}
        />
      )}
    </label>
  );
}
