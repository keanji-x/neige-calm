import { cleanup, render, screen } from "@testing-library/react";
import { page } from "vitest/browser";
import { afterEach, expect, it, vi } from "vitest";

import "../../styles/entry.css";
import type { AnswerAsk } from "./asks.tsx";
import { PlannerAskDrawer } from "./asks.tsx";

afterEach(cleanup);

const TITLE = [
  "合并 PR #12（head 0123abc）？",
  "- 链接：https://github.com/owner/repo/pull/12",
  "- 风险：解析器报错文字变了；接口和存储不变",
  "- CI：success，失败检查：无",
  "- 门禁：implement-12 第 1 次 gate 通过",
  "- 评审：review-12 → 无阻塞发现；未关闭的阻塞发现：无",
  "- 可合并：mergeable",
].join("\n");

// Both option questions and free-text questions show the Planner’s title as a heading.
for (const options of [["合并", "暂不合并"], []]) {
  it(`shows a multi-line question one line per line, as the Planner wrote it (${options.length} options)`, async () => {
    await page.viewport(800, 600);
    render(
      <PlannerAskDrawer
        asks={[{ askId: 1, questions: [{ title: TITLE, options }], delivery: 'wake' }]}
        onAnswer={vi.fn<AnswerAsk>()}
      />,
    );
    const label = screen.getByRole("heading", { name: /^合并 PR #12/ });
    // innerText follows the rendered layout: collapsed whitespace would join the lines with spaces.
    expect(label.innerText).toBe(TITLE);
  });
}

it('scopes softened handle icon tokens to the drawer chrome while keeping ask content icon tokens intact', async () => {
  await page.viewport(800, 600);
  render(
    <PlannerAskDrawer
      asks={[{ askId: 1, questions: [{ title: 'Approve?', options: ['Yes', 'No'] }], delivery: 'wake' }]}
      onAnswer={vi.fn<AnswerAsk>()}
    />,
  );
  const resolveColor = (host: HTMLElement, variable: string) => {
    const probe = document.createElement('span');
    probe.style.color = `var(${variable})`;
    host.appendChild(probe);
    const computed = getComputedStyle(probe).color;
    probe.remove();
    return computed;
  };
  const toggle = screen.getByRole('button', { name: 'Collapse Questions' });
  const asks = document.querySelector<HTMLElement>('[data-nc-asks]')!;
  expect(resolveColor(toggle, '--color-icon-secondary')).toBe(resolveColor(document.documentElement, '--hairline-strong'));
  expect(resolveColor(toggle, '--color-icon-primary')).toBe(resolveColor(document.documentElement, '--text-3'));
  expect(resolveColor(asks, '--color-icon-secondary')).toBe(resolveColor(document.documentElement, '--text-2'));
  expect(resolveColor(asks, '--color-icon-primary')).toBe(resolveColor(document.documentElement, '--text'));
});
