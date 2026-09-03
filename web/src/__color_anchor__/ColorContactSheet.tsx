type ControlKind = 'text' | 'native';

type Row = {
  slug: string;
  tag: 'input' | 'textarea' | 'select';
  controlKind: ControlKind;
  className?: string;
  type?: string;
  placeholder?: string;
  value?: string;
  areaNav?: boolean;
};

const rows: Row[] = [
  { slug: 'bare-input', tag: 'input', controlKind: 'text', type: 'text', placeholder: 'bare' },
  {
    slug: 'schema-form-input',
    tag: 'input',
    controlKind: 'text',
    className: 'schema-form-input',
    type: 'text',
  },
  {
    slug: 'login-input',
    tag: 'input',
    controlKind: 'text',
    className: 'login-input',
    type: 'text',
  },
  {
    slug: 'iframe-url-input',
    tag: 'input',
    controlKind: 'text',
    className: 'iframe-url-input',
    type: 'url',
  },
  {
    slug: 'new-task-form-input',
    tag: 'input',
    controlKind: 'text',
    className: 'new-task-form-input',
    type: 'text',
  },
  {
    slug: 'dirpicker-path-input',
    tag: 'input',
    controlKind: 'text',
    className: 'dirpicker-path-input',
    type: 'text',
  },
  {
    slug: 'track-report-textarea',
    tag: 'textarea',
    controlKind: 'text',
    className: 'track-report-edit-body',
  },
  {
    slug: 'track-title-input',
    tag: 'input',
    controlKind: 'text',
    className: 'track-title-input',
    value: 'Track title',
  },
  {
    slug: 'area-title-input',
    tag: 'input',
    controlKind: 'text',
    className: 'area-title-input',
    value: 'Area title',
  },
  {
    slug: 'area-nav-edit-input',
    tag: 'input',
    controlKind: 'text',
    placeholder: 'New area',
    areaNav: true,
  },
  { slug: 'settings-theme-radio', tag: 'input', controlKind: 'native', type: 'radio' },
  { slug: 'bare-textarea', tag: 'textarea', controlKind: 'text', value: 'bare textarea' },
  { slug: 'bare-select', tag: 'select', controlKind: 'native' },
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
            {row.areaNav ? (
              <div className="side">
                <div className="area-nav-edit">{renderControl(row)}</div>
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

function renderControl({ slug, tag, controlKind, className, type, placeholder, value }: Row) {
  const props = {
    className,
    'data-color-anchor-id': slug,
    'data-color-anchor-kind': controlKind,
    placeholder,
  };
  if (tag === 'textarea') return <textarea {...props} defaultValue={value ?? 'Track report body'} />;
  if (tag === 'select') {
    return (
      <select {...props} defaultValue="one">
        <option value="one">One</option>
        <option value="two">Two</option>
      </select>
    );
  }
  return (
    <input
      {...props}
      defaultChecked={type === 'radio' ? true : undefined}
      defaultValue={value}
      name={type === 'radio' ? 'color-anchor-theme' : undefined}
      type={type ?? 'text'}
    />
  );
}
