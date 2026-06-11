import {
  Children,
  isValidElement,
  type HTMLAttributes,
  type ReactNode,
} from 'react';
import type { Components } from 'react-markdown';

type DirectiveData = {
  hName?: string;
  hProperties?: Record<string, unknown>;
};

type MarkdownPosition = {
  start?: { offset?: number };
  end?: { offset?: number };
};

type MarkdownNode = {
  type: string;
  name?: string;
  attributes?: Record<string, string | null | undefined> | null;
  children?: MarkdownNode[];
  data?: DirectiveData;
  position?: MarkdownPosition;
  value?: string;
};

type MarkdownFile = {
  value?: unknown;
};

type ReportDivProps = HTMLAttributes<HTMLDivElement> & {
  node?: unknown;
  'data-caption'?: string;
  'data-label'?: string;
  'data-stat'?: string;
  'data-unit'?: string;
};

type ReportSupProps = HTMLAttributes<HTMLElement> & {
  node?: unknown;
};

type ReportOlProps = HTMLAttributes<HTMLOListElement> & {
  node?: unknown;
};

function isDirective(node: MarkdownNode): boolean {
  return (
    node.type === 'containerDirective' ||
    node.type === 'leafDirective' ||
    node.type === 'textDirective'
  );
}

function stringAttr(node: MarkdownNode, name: string): string | undefined {
  const value = node.attributes?.[name];
  return typeof value === 'string' ? value : undefined;
}

function setDirectiveElement(
  node: MarkdownNode,
  hProperties: Record<string, unknown>,
): void {
  node.data = node.data ?? {};
  node.data.hName = 'div';
  node.data.hProperties = hProperties;
}

function applyKnownDirective(
  node: MarkdownNode,
  insideFindings: boolean,
): boolean {
  if (node.name === 'findings') {
    if (node.type !== 'containerDirective') return false;
    setDirectiveElement(node, { className: 'findings' });
    return true;
  }

  if (node.name === 'chart') {
    if (node.type !== 'containerDirective' && node.type !== 'leafDirective') {
      return false;
    }

    const label = stringAttr(node, 'label');
    const caption = stringAttr(node, 'caption');
    setDirectiveElement(node, {
      className: 'chart',
      ...(label ? { 'data-label': label } : {}),
      ...(caption ? { 'data-caption': caption } : {}),
    });
    return true;
  }

  if (node.name === 'row') {
    if (
      !insideFindings ||
      (node.type !== 'containerDirective' && node.type !== 'leafDirective')
    ) {
      return false;
    }

    const stat = stringAttr(node, 'stat');
    const unit = stringAttr(node, 'unit');
    setDirectiveElement(node, {
      className: 'find-row',
      ...(stat ? { 'data-stat': stat } : {}),
      ...(unit ? { 'data-unit': unit } : {}),
    });
    return true;
  }

  return false;
}

function directiveSource(node: MarkdownNode, file: MarkdownFile): string {
  const source = typeof file.value === 'string' ? file.value : undefined;
  const start = node.position?.start?.offset;
  const end = node.position?.end?.offset;

  if (
    source &&
    typeof start === 'number' &&
    typeof end === 'number' &&
    start >= 0 &&
    end >= start
  ) {
    return source.slice(start, end);
  }

  const marker =
    node.type === 'containerDirective'
      ? ':::'
      : node.type === 'leafDirective'
        ? '::'
        : ':';
  const attrs = Object.entries(node.attributes ?? {})
    .filter((entry): entry is [string, string] => typeof entry[1] === 'string')
    .map(([key, value]) => `${key}="${value.replaceAll('"', '&quot;')}"`);

  return `${marker}${node.name ?? ''}${attrs.length ? `{${attrs.join(' ')}}` : ''}`;
}

function transformChildren(
  parent: MarkdownNode,
  file: MarkdownFile,
  insideFindings: boolean,
): void {
  if (!parent.children) return;

  for (let index = 0; index < parent.children.length; index += 1) {
    const child = parent.children[index];

    if (isDirective(child)) {
      const known = applyKnownDirective(child, insideFindings);
      if (!known) {
        parent.children[index] = {
          type: 'text',
          value: directiveSource(child, file),
        };
        continue;
      }
    }

    transformChildren(
      child,
      file,
      insideFindings ||
        (child.name === 'findings' && child.type === 'containerDirective'),
    );
  }
}

export function remarkReportDirectives() {
  return function transformReportDirectives(
    tree: MarkdownNode,
    file: MarkdownFile,
  ): void {
    transformChildren(tree, file, false);
  };
}

function hasClass(className: string | undefined, name: string): boolean {
  return className?.split(/\s+/).includes(name) ?? false;
}

function hasRenderableChildren(children: ReactNode): boolean {
  return Children.toArray(children).length > 0;
}

function textContent(node: ReactNode): string {
  if (typeof node === 'string' || typeof node === 'number') return String(node);
  if (Array.isArray(node)) return node.map(textContent).join('');
  if (isValidElement<{ children?: ReactNode }>(node)) {
    return textContent(node.props.children);
  }
  return '';
}

function normalizeFootnoteHref(href: string | undefined, label: string): string {
  if (!href?.startsWith('#')) return `#fn-${label}`;

  const id = href.slice(1).replace(/^user-content-/, '');
  return id.startsWith('fn-') ? `#${id}` : `#fn-${id}`;
}

function footnoteReference(
  children: ReactNode,
): { href: string; label: string } | null {
  const child = Children.toArray(children)[0];
  if (
    !isValidElement<{
      children?: ReactNode;
      href?: string;
      'data-footnote-ref'?: boolean;
    }>(child) ||
    !child.props['data-footnote-ref']
  ) {
    return null;
  }

  const label = textContent(child.props.children).trim();
  const href = normalizeFootnoteHref(child.props.href, label || '1');
  const fallbackLabel = href.replace(/^#fn-/, '');
  return { href, label: label || fallbackLabel };
}

function hasFootnoteItem(children: ReactNode): boolean {
  return Children.toArray(children).some(
    (child) =>
      isValidElement<{ id?: string }>(child) &&
      typeof child.props.id === 'string' &&
      /^fn-/.test(child.props.id.replace(/^user-content-/, '')),
  );
}

function joinClassNames(...parts: Array<string | undefined>): string | undefined {
  const className = parts.filter(Boolean).join(' ');
  return className || undefined;
}

function ReportDiv({
  node: _node,
  className,
  children,
  'data-caption': caption,
  'data-label': label,
  'data-stat': stat,
  'data-unit': unit,
  ...rest
}: ReportDivProps) {
  if (hasClass(className, 'findings')) {
    return (
      <div className="findings">
        <div className="fh">
          <svg
            aria-hidden="true"
            className="star"
            focusable="false"
            viewBox="0 0 24 24"
          >
            <path d="M12 3l2.5 5.5L20 9.5l-4 4 1 6-5-3-5 3 1-6-4-4 5.5-1L12 3z" />
          </svg>
          <span>Key findings</span>
        </div>
        {children}
      </div>
    );
  }

  if (hasClass(className, 'find-row')) {
    return (
      <div className="find-row">
        <div className="find-stat">
          {stat}
          {unit ? <span className="u">{unit}</span> : null}
        </div>
        <div className="find-txt">{children}</div>
      </div>
    );
  }

  if (hasClass(className, 'chart')) {
    return (
      <>
        <div
          aria-label={label ? `Chart placeholder: ${label}` : 'Chart placeholder'}
          className="chart"
          role="img"
        >
          {label ? <div className="ph-tag">{label}</div> : null}
          {caption ? <div className="cap">{caption}</div> : null}
        </div>
        {hasRenderableChildren(children) ? (
          <div className="figcap">{children}</div>
        ) : null}
      </>
    );
  }

  return (
    <div {...rest} className={className}>
      {children}
    </div>
  );
}

function ReportSup({ node: _node, children, ...rest }: ReportSupProps) {
  const ref = footnoteReference(children);
  if (ref) {
    return (
      <sup>
        <a className="report-ref" href={ref.href}>
          [{ref.label}]
        </a>
      </sup>
    );
  }

  return <sup {...rest}>{children}</sup>;
}

function ReportOl({ node: _node, children, className, ...rest }: ReportOlProps) {
  return (
    <ol
      {...rest}
      className={joinClassNames(
        className,
        hasFootnoteItem(children) ? 'report-footnotes' : undefined,
      )}
    >
      {children}
    </ol>
  );
}

export const reactMarkdownComponents: Components = {
  div: ReportDiv,
  ol: ReportOl,
  sup: ReportSup,
};
