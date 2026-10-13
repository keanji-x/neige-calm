import { isHtmlPagePath } from '../../../../core/domain/report-html.ts';

const IMAGE_EXTENSIONS = Object.freeze([
  '.png', '.jpg', '.jpeg', '.gif', '.webp', '.bmp', '.ico', '.svg',
] as const);

export function isImagePath(path: string): boolean {
  const lower = path.toLowerCase();
  return IMAGE_EXTENSIONS.some((extension) => lower.endsWith(extension));
}

export function isMarkdownPath(path: string): boolean {
  const lower = path.toLowerCase();
  return lower.endsWith('.md') || lower.endsWith('.markdown');
}

/** A page the Report frames, by the html block's one page rule (`core/domain/report-html.ts`); the card browser shows it as source. */
export function isHtmlPath(path: string): boolean {
  return isHtmlPagePath(path);
}
