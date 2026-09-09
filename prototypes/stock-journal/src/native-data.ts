import { portfolioTemplateBlocks } from './native-template';
import type { ReportBlock } from '../../../fe/core/domain/report.ts';
import type { TrackWire } from '../../../fe/core/domain/track.ts';

const timestamp = Date.parse('2026-09-09T00:00:00+08:00');
export const previewArea = { id: 'investments', name: '个人投资研究', color: '#849188', sort: 1, kind: 'user', created_at: timestamp, updated_at: timestamp };
const prose = (id: string, markdown: string): ReportBlock => ({ id, kind: 'prose', payload: { markdown } });
type PreviewReport = { schemaVersion: 3; docRev: number; summary: string; body: string; blocks: ReportBlock[] };
type PreviewTrack = { track: TrackWire; report: PreviewReport };

function track(id: string, title: string, sort: number, blocks: ReportBlock[]): PreviewTrack {
  return {
    track: { id, title, area_id: previewArea.id, sort, lifecycle: 'working', cwd: '/investment-preview', archived_at: null,
      pinned_at: id === 'portfolio' ? timestamp : null, terminal_at: null, created_at: timestamp, updated_at: timestamp },
    report: { schemaVersion: 3, docRev: 1, summary: '原生 Neige Report · 虚构投资研究示例',
      body: blocks.filter(block => block.kind === 'prose').map(block => (block.payload as { markdown: string }).markdown).join('\n\n'), blocks },
  };
}

export function createPreviewTracks(): PreviewTrack[] {
  return [
    track('portfolio', '个人投资 · 管理总览', 0, portfolioTemplateBlocks()),
    track('pine', '青松科技 · 投资档案', 1, [
      prose('pine-thesis', '# 投资逻辑\n\n**从一次性交付走向持续性收入，续费业务的质量比短期利润增速更值得关注。**\n\n当前仓位 18%，示例市值 ¥36,000。我的判断是继续持有，等待经营现金流进一步验证。\n\n[返回持仓总览](neige://wave/portfolio#holdings-heading) · [查看本次决策记录](neige://wave/journal#decision-log)\n\n> 以下公司、财务数据与报告均为虚构，用于演示 Neige Report 的阅读、引用与资料打开流程。'),
      prose('pine-report', '# 半年报精读\n\n## 收入的确定性，正在提高\n\n本期续费收入占比由 58% 提高至 64%，核心客户续费率由 88% 提高至 89%。业务结构的变化提高了收入的可预测性，但单个报告期还不足以确认长期趋势。\n\n## 好的利润，需要现金流验证\n\n经营现金流与净利润的比值由 81% 降至 76%。应收款的增加需要进一步解释：这是季节性结算，还是客户账期正在拉长？下一季度先看回款，不单独外推利润增长。'),
      { id: 'pine-evidence', kind: 'table', payload: { caption: '证据对照 · 2026 半年报示例', columns: [
        { key: 'metric', label: '观察指标' }, { key: 'before', label: '上期', align: 'right' },
        { key: 'now', label: '本期', align: 'right' }, { key: 'meaning', label: '我的解读' },
      ], rows: [
        { metric: '续费收入占比', before: '58%', now: '64%', meaning: '收入结构改善' },
        { metric: '核心客户续费率', before: '88%', now: '89%', meaning: '客户留存稳定' },
        { metric: '现金流 / 净利润', before: '81%', now: '76%', meaning: '需要核对回款质量' },
      ] } },
      prose('pine-invalidation', '# 什么情况会改变判断\n\n出现以下任一情况，重新评估原有投资逻辑：\n\n- 连续两个季度，经营现金流低于净利润的 70%。\n- 核心客户续费率低于 85%。\n- 应收账款账龄持续拉长，且没有合理的经营解释。'),
      prose('pine-sources', '# 资料与复盘\n\n**下次复盘：2026 年 9 月 12 日。**\n\n- [打开半年报摘录](./research/pine-half-year.md)\n- [打开现金流跟踪笔记](./research/pine-cashflow.md)\n\n文件会在 Neige 原生文件阅读器中打开，阅读后可返回当前 Report。'),
    ]),
    track('bay', '海湾消费 · 投资档案', 2, [
      prose('bay-thesis', '# 投资逻辑\n\n**渠道去库存接近尾声，减少促销后的自然复购是利润恢复的前提。**\n\n当前仓位 12%，示例市值 ¥24,000。库存改善提供了线索，还不能直接当作终端需求反转的证据。\n\n[返回持仓总览](neige://wave/portfolio#holdings-heading)'),
      prose('bay-evidence', '# 渠道观察\n\n库存周转由 57 天缩短至 48 天，复购率从 31% 升至 32%。促销销量占比仍然达到 41%，需要继续区分补贴带来的销量与自然需求。\n\n## 判断失效条件\n\n去库存结束后，终端复购连续两个季度下降。\n\n## 下次复盘\n\n2026 年 9 月 18 日，核对终端售价、非促销销量与经销商补货意愿。'),
      prose('bay-sources', '# 研究来源\n\n[打开渠道调研记录](./research/bay-channel.md)\n\n本文及来源均为虚构示例。'),
    ]),
    track('river', '远川工业 · 投资档案', 3, [
      prose('river-thesis', '# 投资逻辑\n\n**设备更新带来订单机会，先验证交付、验收与收款节奏。**\n\n当前仓位 10%，示例市值 ¥20,000。订单、收入和现金流分别跟踪，避免把签单直接视为业绩兑现。\n\n[返回持仓总览](neige://wave/portfolio#holdings-heading)'),
      prose('river-evidence', '# 经营跟踪\n\n在手订单同比增长 21%，按期交付率由 94% 回落至 92%，预收款增长 12%。需求可见度改善，但交付效率仍然需要验证。\n\n## 判断失效条件\n\n主要订单延期超过两个季度，或预收款持续下降。\n\n## 下次复盘\n\n2026 年 9 月 25 日，核对项目验收节点、产能排期和预收款。'),
      prose('river-sources', '# 研究来源\n\n[打开订单与交付笔记](./research/river-orders.md)\n\n本文及来源均为虚构示例。'),
    ]),
    track('journal', '投资决策 · 记录与复盘', 4, [
      prose('decision-log', '# 2026.09.06 · 青松科技\n\n**决定：继续持有，等待回款验证。**\n\n收入结构有所改善，但现金流证据不够充分。维持 18% 的示例仓位，下一次复盘优先核对实际回款。\n\n依据：[青松科技半年报精读](neige://wave/pine#pine-report)。\n\n改变判断的条件：核心客户续费率低于 85%，或连续两季现金流低于净利润的 70%。'),
      prose('journal-template', '# 每次决定，都留下这四件事\n\n1. **决定是什么**：买入、继续持有、减仓，或保持观察。\n2. **依据是什么**：引用对应股票 Report 的具体段落和源文件。\n3. **哪里可能错**：写出明确、可验证的判断失效条件。\n4. **什么时候复盘**：把下一个验证节点留在报告里。\n\n[返回持仓与研究总览](neige://wave/portfolio#review-plan)\n\n> 这是只读原型，以上记录为示例。真实写入沿用 Neige 对话更新 Report 的流程；本预览不会启动模型或执行任务。'),
    ]),
  ];
}

export const sourceFiles: Readonly<Record<string, string>> = {
  'research/pine-half-year.md': '# 青松科技 · 2026 半年报摘录\n\n> 虚构来源，仅用于预览。\n\n## 收入结构\n\n续费收入占比从 58% 提高至 64%，核心客户续费率为 89%。\n\n## 现金流\n\n经营现金流与净利润比值为 76%，低于上期的 81%。\n\n## 我的批注\n\n收入可预测性改善，回款仍待验证。\n\n[查看现金流跟踪](./pine-cashflow.md)',
  'research/pine-cashflow.md': '# 青松科技 · 回款质量跟踪\n\n> 虚构研究笔记。\n\n应收款同比增长 24%，收入同比增长 18%；回款周期由 64 天延长到 72 天。\n\n下一步：对比应收账龄、合同负债和经营现金流。',
  'research/bay-channel.md': '# 海湾消费 · 渠道调研\n\n> 虚构调研记录。\n\n库存周转 48 天，复购率 32%，促销销量占比 41%。\n\n待验证：促销减少后，自然复购能否保持。',
  'research/river-orders.md': '# 远川工业 · 订单与交付\n\n> 虚构研究记录。\n\n在手订单增长 21%，按期交付率 92%。\n\n待验证：大额项目验收、预收款和产能安排。',
};
