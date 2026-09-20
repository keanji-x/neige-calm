/** The source panel's words, declared once. The provenance badge wording is owner-signed and must not be paraphrased. */

import type { SourceProvenance } from '../../../../../core/domain/report-source.ts';

export const SOURCE_PROVENANCE_COPY = Object.freeze({
  full_text: '智堡全文',
  summary: '智堡摘要，非机构原文',
  web_page: '网页',
  manual: '手工录入，未经内核核验',
} satisfies Record<SourceProvenance, string>);

export const SOURCE_PANEL_COPY = Object.freeze({
  /** The drawer's name before the row has arrived, and the citation badge. */
  panelTitle: '来源',
  closeLabel: '关闭来源',
  loading: '正在读取来源…',
  /** A citation the track cannot answer: dangling id, or a link that will not parse. */
  missingTitle: '来源缺失',
  missingDangling: '本 track 里没有这条来源。配方生成的 track、或来源被指向别的 track 时会出现这种情况。',
  missingMalformed: '这条引用的链接格式不合法，无法定位来源。',
  destinationLabel: '链接',
  /** The source exists but the anchor does not: the source is still shown underneath, but the notice is the missing state. */
  anchorMissingTitle: '引用锚点缺失',
  anchorMissingDetail: '这条引用指向的锚点不在来源里（锚点可能尚未追加，或其文本不在正文中），下面显示的是完整正文。',
  publishedAt: '发布',
  capturedAt: '抓取',
  origin: '来源',
  originPlugin: '插件',
  originTool: '工具',
  contentId: '内容 ID',
  url: 'URL',
  /** The inline citation on surfaces without a Track or source-panel handler. */
  citationBadge: '来源',
} as const);
