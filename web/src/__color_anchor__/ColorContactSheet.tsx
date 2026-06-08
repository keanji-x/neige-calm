type Row = { slug: string; tag: 'input' | 'textarea' | 'select'; className?: string; type?: string; placeholder?: string; value?: string; coveNav?: boolean };

const rows: Row[] = [
  { slug: 'bare-input', tag: 'input', type: 'text', placeholder: 'bare' },
  { slug: 'schema-form-input', tag: 'input', className: 'schema-form-input', type: 'text' },
  { slug: 'login-input', tag: 'input', className: 'login-input', type: 'text' },
  { slug: 'iframe-url-input', tag: 'input', className: 'iframe-url-input', type: 'url' },
  { slug: 'new-task-form-input', tag: 'input', className: 'new-task-form-input', type: 'text' },
  { slug: 'dirpicker-path-input', tag: 'input', className: 'dirpicker-path-input', type: 'text' },
  { slug: 'wave-report-textarea', tag: 'textarea', className: 'wave-report-edit-body' },
  { slug: 'wave-title-input', tag: 'input', className: 'wave-title-input', value: 'Wave title' },
  { slug: 'cove-title-input', tag: 'input', className: 'cove-title-input', value: 'Cove title' },
  { slug: 'cove-nav-edit-input', tag: 'input', placeholder: 'New cove', coveNav: true },
  { slug: 'settings-theme-radio', tag: 'input', type: 'radio' },
  { slug: 'bare-textarea', tag: 'textarea', value: 'bare textarea' },
  { slug: 'bare-select', tag: 'select' },
];

const sheetStyle = { maxWidth: 560, padding: 24 };
const labelStyle = { display: 'flex', flexDirection: 'column', gap: 8, marginBottom: 18 } as const;

export function ColorContactSheet() {
  return (
    <div className="calm-shell">
      <main style={sheetStyle}>
        {rows.map((row) => (
          <label key={row.slug} style={labelStyle}>
            <span>{row.slug}</span>
            {row.coveNav ? (
              <div className="side">
                <div className="cove-nav-edit">{renderControl(row)}</div>
              </div>
            ) : (
              renderControl(row)
            )}
          </label>
        ))}
      </main>
    </div>
  );
}

function renderControl({ slug, tag, className, type, placeholder, value }: Row) {
  const props = { className, 'data-color-anchor-id': slug, placeholder };
  if (tag === 'textarea') return <textarea {...props} defaultValue={value ?? 'Wave report body'} />;
  if (tag === 'select') {
    return (
      <select {...props} defaultValue="one">
        <option value="one">One</option>
        <option value="two">Two</option>
      </select>
    );
  }
  return <input {...props} defaultChecked={type === 'radio' ? true : undefined} defaultValue={value} name={type === 'radio' ? 'color-anchor-theme' : undefined} type={type ?? 'text'} />;
}
