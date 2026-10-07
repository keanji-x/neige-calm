import { cleanup, render, screen } from "@testing-library/react";
import { page } from "vitest/browser";
import { afterEach, expect, it, vi } from "vitest";

import "../../styles/entry.css";
import type { AnswerAsk } from "./asks.tsx";
import { PlannerAskDrawer } from "./asks.tsx";

afterEach(cleanup);

const TITLE = [
  "合并 PR #12（head 0123abc）？",
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
        asks={[{ askId: 1, questions: [{ title: TITLE, options }] }]}
        onAnswer={vi.fn<AnswerAsk>()}
      />,
    );
    const label = screen.getByRole("heading", { name: /^合并 PR #12/ });
    // innerText follows the rendered layout: collapsed whitespace would join the lines with spaces.
    expect(label.innerText).toBe(TITLE);
  });
}
