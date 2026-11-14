import test from "node:test";
import assert from "node:assert/strict";
import {
  reportAt,
  detailAt,
  verdictsAt,
  TASK_KEY,
  documentText,
} from "../live/state.ts";
test("scripted report and document streams grow deterministically", () => {
  for (const [start, end] of [
    [1, 3],
    [10, 14],
  ]) {
    let prior = 0;
    for (let t = start; t <= end; t += 0.1) {
      const current =
        start === 1 ? reportAt(t).markdown.length : reportAt(t).document.length;
      assert.ok(current >= prior);
      prior = current;
    }
  }
  assert.equal(reportAt(22).document, documentText);
  assert.deepEqual(reportAt(18), reportAt(18));
});
test("decision precedes committed artifacts and completion", () => {
  assert.match(reportAt(6).markdown, /等你决定/);
  assert.match(reportAt(13).markdown, /本次决定/);
  assert.doesNotMatch(reportAt(14).markdown, /成果入口/);
  assert.match(reportAt(22).markdown, /成果入口/);
  assert.equal(verdictsAt(15)[0].status, "running");
  assert.equal(verdictsAt(25)[0].status, "done");
  assert.match(reportAt(25).markdown, /已完成/);
});
test("all runtime projections share stable identifiers and task declaration", () => {
  for (const t of [0, 8, 15, 23, 30]) {
    const detail = detailAt(t);
    assert.equal(detail.track.id, "11111111111141118111111111111111");
    const report = detail.cards[1];
    assert.equal(report.id, "33333333333343338333333333333333");
    if (t >= 4.5) assert.equal(verdictsAt(t)[0].key, TASK_KEY);
  }
  const blocks = detailAt(30).cards[1].payload.blocks;
  assert.ok(blocks);
  assert.equal(blocks.at(-1)?.kind, "app");
});

test("thought text enters the real report only at landing", async () => {
  const { thoughtAt } = await import("../live/state.ts");
  assert.equal(thoughtAt(1.3).flight, 0);
  assert.ok(thoughtAt(1.7).flight > 0);
  assert.doesNotMatch(reportAt(1.89).markdown, /先确认主题方式，/);
  assert.match(reportAt(1.91).markdown, /先确认主题方式，/);
  assert.equal(thoughtAt(1.91).landed, true);
  assert.equal(thoughtAt(3.1).visible, false);
  assert.match(reportAt(14).markdown, /使用说明和页面一起交付/);
});

test("the new-track composer starts with a character and grows in phrases", async () => {
  const { typed, REQUEST } = await import("../live/intro.ts");
  assert.equal(typed(REQUEST, 4.9, 5, 7.8), "");
  assert.equal(typed(REQUEST, 5.1, 5, 7.8), "给");
  assert.equal(typed(REQUEST, 8, 5, 7.8), REQUEST);
});
test("the conversation keeps the request, question and user decision in order", async () => {
  const { transcriptAt, REQUEST } = await import("../live/intro.ts");
  assert.equal(transcriptAt(0).length, 1);
  assert.equal(transcriptAt(6).length, 2);
  const rows = transcriptAt(10);
  assert.equal(rows.length, 3);
  assert.equal(JSON.parse(rows[0].params).item.content[0].text, REQUEST);
  assert.equal(
    JSON.parse(rows[1].params).item.text,
    "跟随系统，还是手动切换？",
  );
  assert.equal(JSON.parse(rows[2].params).item.content[0].text, "两种都要。");
});

test("current conversation envelope keeps Human authors separate from native agent items", async () => {
  const { conversationAt } = await import("../live/intro.ts");
  const page = conversationAt(10);
  assert.equal(page.contract, "conversation_v1");
  assert.equal(page.items[0].source.kind, "human");
  assert.equal(page.items[1].source.kind, "native");
  assert.equal(page.items[2].source.kind, "human");
  assert.equal(detailAt(10).track.workspace.isolation, "managed");
  assert.equal(detailAt(10).can_admin, false);
});
