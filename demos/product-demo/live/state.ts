export const TRACK = "11111111111141118111111111111111",
  AREA = "22222222222242228222222222222222",
  REPORT = "33333333333343338333333333333333",
  PLANNER = "44444444444444448444444444444444";
export const thoughtTracks = [
  {
    start: 1,
    end: 3,
    chunks: [
      { thought: "先确认主题方式。", text: "先确认主题方式，" },
      { thought: "再实现切换与偏好保存。", text: "再实现切换与偏好保存。\n\n" },
    ],
  },
  {
    start: 10,
    end: 14,
    chunks: [
      {
        thought: "系统、浅色、深色都保留。",
        text: "打开页面预览，直接试用小店。\n\n- 系统、浅色、深色三种外观\n",
      },
      {
        thought: "手动切换，也能恢复跟随系统。",
        text: "- 手动切换优先，支持恢复跟随系统\n",
      },
      {
        thought: "使用说明和页面一起交付。",
        text: "- 使用说明和页面一起交付\n\n",
      },
    ],
  },
];
const landing = 0.9;
export function committedThoughts(
  t: number,
  track: (typeof thoughtTracks)[number],
) {
  const step = (track.end - track.start) / track.chunks.length;
  return track.chunks
    .filter((_, i) => t >= track.start + (i + landing) * step)
    .map((c) => c.text)
    .join("");
}
export function thoughtAt(t: number) {
  const track = thoughtTracks.find((s) => t >= s.start && t < s.end);
  if (!track)
    return { visible: false, text: "", phase: 0, flight: 0, landed: false };
  const u =
    ((t - track.start) / (track.end - track.start)) * track.chunks.length;
  const index = Math.min(track.chunks.length - 1, Math.floor(u)),
    phase = u - index;
  return {
    visible: true,
    text: track.chunks[index].thought,
    phase,
    flight: Math.max(0, Math.min(1, (phase - 0.54) / (landing - 0.54))),
    landed: phase >= landing,
  };
}
export const TASK_KEY = "暗色模式";
export const documentText =
  "# 像素小店使用说明\n\n## 如何使用\n1. 在当前 Track 中找到小店预览。\n2. 点击深色外观，观察背景与商品卡片的变化。\n3. 也可以切回浅色，或恢复跟随系统。\n\n## 外观切换\n提供系统、浅色和深色三种模式。在独立页面中可保存手动选择；预览隔离可能限制浏览器存储。\n\n## 接着做\n方案、页面、说明与检查结果，都保留在同一条工作主线里。";
export function reportAt(t: number) {
  const goal = "## 目标\n给小店页面加上暗色模式，做好后直接试用。\n\n";
  const plan =
    t >= 1
      ? "## 先把方案说清楚\n" + committedThoughts(t, thoughtTracks[0])
      : "";
  const choice =
    t >= 4.5 && t < 9
      ? "## 等你决定\n跟随系统，还是手动切换？\n\n"
      : t >= 9
        ? "## 本次决定\n默认跟随系统，同时保留手动切换。\n\n"
        : "";
  const work =
    t >= 18
      ? "## 已完成\n页面切换与使用说明已经交付。\n\n"
      : t >= 9
        ? "## 正在推进\n页面切换与偏好保存正在一起完成。\n\n"
        : "";
  const doc =
    t >= 10 ? "## 使用说明\n" + committedThoughts(t, thoughtTracks[1]) : "";
  const links =
    t >= 15
      ? "## 成果入口\n[使用说明](使用说明.md) · [检查结果](检查结果.md)\n\n"
      : "";
  return {
    markdown: goal + plan + choice + work + doc + links,
    document:
      t >= 13.9
        ? documentText
        : documentText.slice(
            0,
            Math.floor(
              documentText.length * Math.max(0, Math.min(1, (t - 10) / 4)),
            ),
          ),
    finished: t >= 18,
    preview: t >= 20,
  };
}
export function detailAt(t: number) {
  const state = reportAt(t),
    task = {
      id: "task-theme",
      kind: "task",
      rev: 1,
      payload: {
        key: TASK_KEY,
        kind: "codex",
        goal: "实现三态外观与使用说明",
        ready: true,
        declared_by: "spec",
        no_gate_reason: "前端脚本演示，不执行后台任务",
      },
    };
  return {
    track: {
      id: TRACK,
      area_id: AREA,
      title: "小店页面 · 暗色模式",
      sort: 0,
      cwd: "/demo/pixel-shop",
      pinned_at: null,
      closed_at: null,
      admin_granted_at: null,
      template_id: null,
      plugin_scope: null,
      purpose: null,
      workspace: {
        isolation: "managed",
        kind: "managed",
        path: "/demo/pixel-shop",
        frozen_at: null,
      },
      created_at: 1,
      updated_at: 2,
    },
    can_admin: false,
    can_reopen: false,
    can_close: true,
    cards: [
      {
        id: PLANNER,
        track_id: TRACK,
        kind: "codex",
        title: "Planner",
        sort: 1,
        payload: { planner_provider: "codex", planner_harness: true },
        deletable: false,
        created_at: 1,
        updated_at: 2,
      },
      {
        id: REPORT,
        track_id: TRACK,
        kind: "track-report",
        title: null,
        sort: -1,
        payload: {
          schemaVersion: 3,
          docRev: Math.floor(t * 12),
          summary: "小店暗色模式",
          body: state.markdown,
          blocks: [
            {
              id: "main-prose",
              kind: "prose",
              rev: 1,
              payload: { markdown: state.markdown },
            },
            ...(t >= 4.5 ? [task] : []),
            ...(state.preview
              ? [
                  {
                    id: "site-preview",
                    kind: "app",
                    rev: 1,
                    payload: {
                      src: "/sample/index.html",
                      title: "小店页面",
                      height: 570,
                    },
                  },
                ]
              : []),
          ],
        },
        deletable: false,
        created_at: 1,
        updated_at: 2,
      },
    ],
    overlays: [],
  };
}

export function verdictsAt(t: number) {
  return t >= 4.5
    ? [
        {
          blockId: "task-theme",
          key: TASK_KEY,
          schedulable: true,
          status: t >= 18 ? "done" : "running",
          statusDetail: null,
          workerCardId: null,
          pendingReason: null,
        },
      ]
    : [];
}
