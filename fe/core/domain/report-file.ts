/**
 * A file destination authored inside report Markdown.
 *
 * Relative links stay relative; an absolute link is only a candidate until the
 * current Track's workspace root admits it. Reports are agent-authored input,
 * so URLs and paths outside that root must never become local-file reads merely
 * because their visible labels looked harmless.
 */
export type ReportFileLinkTarget = Readonly<{ path: string }>;

function normalizePath(
  path: string,
  allowAbsolute: boolean,
  allowLeadingParent = false,
): ReportFileLinkTarget | null {
  if (
    path === ''
    || path.includes('\0')
    || path.includes('\\')
    || path.includes('?')
    || path.includes('#')
    || path.startsWith('//')
    || (!allowAbsolute && path.startsWith('/'))
    || /^[A-Za-z][A-Za-z0-9+.-]*:/.test(path)
  ) return null;
  const absolute = path.startsWith('/');
  const parts: string[] = [];
  for (const part of path.split('/')) {
    if (part === '' || part === '.') continue;
    if (part === '..') {
      if (parts.length > 0 && parts.at(-1) !== '..') parts.pop();
      else if (!absolute && allowLeadingParent) parts.push('..');
      else return null;
      continue;
    }
    parts.push(part);
  }
  if (parts.length === 0 || parts.every((part) => part === '..')) return null;
  return { path: `${absolute ? '/' : ''}${parts.join('/')}` };
}

function destinationPath(destination: string): string | null {
  let decoded: string;
  try {
    decoded = decodeURIComponent(destination);
  } catch {
    return null;
  }
  if (decoded.includes('?')) return null;
  const withoutFragment = decoded.split('#', 1)[0] ?? '';
  const filePath = withoutFragment.startsWith('file://')
    ? withoutFragment.slice('file://'.length)
    : withoutFragment;
  if (
    filePath === ''
    || (filePath !== withoutFragment && !filePath.startsWith('/'))
  ) return null;
  return filePath.replace(/:\d+(?::\d+)?$/, '');
}

/** Parse and normalize one candidate; root admission happens separately. */
export function parseReportFileLink(destination: string): ReportFileLinkTarget | null {
  const path = destinationPath(destination);
  return path === null ? null : normalizePath(path, true, true);
}

/** Parse the relative spelling stored in route state and recent-file history. */
export function parseWorkspaceRelativeFilePath(destination: string): ReportFileLinkTarget | null {
  return normalizePath(destination, false);
}

function normalizedRoot(workspaceRoot: string): string | null {
  if (!workspaceRoot.startsWith('/') || workspaceRoot.includes('\0')) return null;
  const parts: string[] = [];
  for (const part of workspaceRoot.split('/')) {
    if (part === '' || part === '.') continue;
    if (part === '..') {
      if (parts.pop() === undefined) return null;
    } else {
      parts.push(part);
    }
  }
  return `/${parts.join('/')}`;
}

/** Return the stable workspace-relative spelling for a safe target. */
export function reportFilePathRelativeToRoot(
  workspaceRoot: string,
  target: ReportFileLinkTarget,
  basePath = '',
): string | null {
  const root = normalizedRoot(workspaceRoot);
  const parsed = normalizePath(target.path, true, true);
  if (root === null || parsed === null) return null;
  if (parsed.path.startsWith('/')) {
    if (root === '/') return parsed.path.slice(1) || null;
    const prefix = `${root}/`;
    return parsed.path.startsWith(prefix) ? parsed.path.slice(prefix.length) : null;
  }
  const base = basePath === '' ? '' : parseWorkspaceRelativeFilePath(basePath)?.path;
  if (base === undefined) return null;
  return normalizePath(base === '' ? parsed.path : `${base}/${parsed.path}`, false)?.path ?? null;
}

/** Resolve a parsed target without letting the browser invent path semantics. */
export function resolveReportFilePath(
  workspaceRoot: string,
  target: ReportFileLinkTarget,
  basePath = '',
): string | null {
  const root = normalizedRoot(workspaceRoot);
  const relative = reportFilePathRelativeToRoot(workspaceRoot, target, basePath);
  if (root === null || relative === null) return null;
  return root === '/' ? `/${relative}` : `${root}/${relative}`;
}
