/**
 * The source panel's words, declared once (#1669 §2.5).
 *
 * The provenance badge is the one line on this surface the owner signed the
 * wording of: `summary` must say out loud that it is 智堡's summary and not
 * the institution's text, and `manual` must say the kernel never verified
 * it. Both are claims about *trust*, and a surface that paraphrased them
 * would be making a different claim. So the four strings live here, keyed
 * by the wire enum, and `satisfies Record<SourceProvenance, string>` is what
 * makes a fifth provenance a type error here rather than an empty badge on
 * the page.
 */

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
  /** The anchor names a quote the row does not carry, or its text is not in the body. */
  anchorMissed: '锚点未命中：这条引用指向的片段不在来源正文里，下面显示的是完整正文。',
  publishedAt: '发布',
  capturedAt: '抓取',
  origin: '来源',
  originPlugin: '插件',
  originTool: '工具',
  contentId: '内容 ID',
  url: 'URL',
  /** The inline citation on surfaces that cannot open the panel (narrow viewports). */
  citationBadge: '来源',
} as const);
