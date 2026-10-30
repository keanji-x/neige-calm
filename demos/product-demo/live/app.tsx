import { RelayCompanion } from "./RelayCompanion";
import { contentTimeAt, relayAt } from "./relay-model";
import ReportWorker from "@neige/features/report/document/preparation.worker.ts?worker";
import { runWorkerTask } from "@neige/systems/worker-task/public.ts";
import { trackDetailSchema } from "@core/domain/track.ts";
import { WEB_COMPAT_VERSION } from "@neige/app/providers/public.tsx";
import { DeliveryCompanion } from "./DeliveryCompanion";
import {
  INTRO,
  AREA_NAME,
  REQUEST,
  named,
  fill,
  typed,
  introAt,
  settle,
  transcriptAt,
  conversationAt,
} from "./intro";
import { ThoughtCompanion } from "./ThoughtCompanion";
import { createRoot } from "react-dom/client";
import { flushSync } from "react-dom";
import {
  QueryClient,
  QueryClientProvider,
  notifyManager,
} from "@tanstack/react-query";
import { RouterProvider, createMemoryHistory } from "@tanstack/react-router";
import "@neige/styles/entry.css";
import { createAppRouter } from "@neige/app/router/public.tsx";
import { ThemeProvider } from "@neige/app/theme/public.tsx";
import { createUnauthorizedChannel } from "@core/api/unauthorized.ts";
import { queryKeys } from "@neige/app/providers/queries.ts";
import {
  createCardHost,
  createCardRegistry,
} from "@neige/systems/cards/public.js";
import { bootCards } from "@neige/app/cards.ts";
import {
  detailAt,
  reportAt,
  documentText,
  TRACK,
  AREA,
  PLANNER,
  verdictsAt,
} from "./state";
let time = 0;
let areaCreated = false,
  trackCreated = false;
const area = {
  id: AREA,
  name: AREA_NAME,
  color: "#8e86bd",
  sort: 0,
  kind: "user",
  default_template_id: null,
  default_cwd: null,
  created_at: 1,
  updated_at: 1,
};
const requests: any[] = [];
const receipts = new Map<string, any>();
const client = new QueryClient({
  defaultOptions: { queries: { retry: false, staleTime: Infinity } },
});
notifyManager.setScheduler((cb) => queueMicrotask(cb));
const ok = (body: unknown) => ({ status: 200, statusText: "OK", body });
const transport = {
  send: async (request: any) => {
    requests.push({
      method: request.method,
      path: request.path,
      ...(request.method === "GET" ? {} : { body: request.body }),
    });
    const p = request.path;
    const receiptMatch =
      /^\/api\/read-receipts\/(track|conversation)\/([^/]+)\/read$/.exec(p);
    if (
      request.method === "POST" &&
      receiptMatch &&
      [TRACK, PLANNER].includes(receiptMatch[2])
    ) {
      const value = {
        kind: receiptMatch[1],
        targetId: receiptMatch[2],
        readThroughMs: request.body.readThroughMs,
        manualUnread: false,
      };
      receipts.set(p, value);
      return ok(value);
    }
    if (request.method === "POST" && p === "/api/areas") {
      if (request.body.name !== AREA_NAME)
        throw Error("Only the isolated sample Area is allowed");
      areaCreated = true;
      return ok(area);
    }
    if (request.method === "POST" && p === "/api/tracks") {
      if (
        request.body.area_id !== AREA ||
        request.body.first_message !== REQUEST
      )
        throw Error("Only the scripted sample Track is allowed");
      trackCreated = true;
      return ok(detailAt(time).track);
    }
    if (
      request.method === "POST" &&
      p === `/api/cards/${PLANNER}/planner/input`
    ) {
      if (request.body.text !== "两种都要。") throw Error("Unscripted input");
      answerSent = true;
      return ok({
        card_id: PLANNER,
        worker_session_id: "demo-only",
        entry_id: "demo-answer",
      });
    }
    if (request.method !== "GET") throw Error("演示拒绝未编排的写入");
    if (p === "/api/version")
      return ok({
        areaCreateIdempotency: true,
        assistantProviders: true,
        webCompatVersion: WEB_COMPAT_VERSION,
        minWebCompatVersion: WEB_COMPAT_VERSION,
        syncEventVersion: 1,
        dbInstanceId: "demo-only",
        databaseId: "demo-only",
      });
    if (p === "/api/read-receipts")
      return ok({
        baselineMs: 1792454400000,
        receipts: [...receipts.values()],
      });
    if (p.startsWith("/api/models"))
      return ok({
        models: [],
        default: {
          model: null,
          reasoning_effort: null,
          supported_reasoning_efforts: null,
        },
        default_source: "unknown",
        source: "unavailable",
        fetched_at_ms: null,
      });
    if (p === "/api/areas") return ok(areaCreated ? [area] : []);
    if (p === `/api/areas/${AREA}/tracks`)
      return ok(trackCreated ? [detailAt(time).track] : []);
    if (p === `/api/tracks/${TRACK}`) return ok(detailAt(time));
    if (p === `/api/tracks/${TRACK}/report`)
      return ok({ taskDiagnostics: verdictsAt(time) });
    if (p.includes("/workspace/readfile")) {
      const file = new URL(p, "http://demo").searchParams.get("path");
      return ok({
        path: "/demo/pixel-shop/" + file,
        size: documentText.length,
        text:
          file === "使用说明.md"
            ? documentText
            : "# 检查结果\n\n这是前端编排演示，不代表一次新执行的后台检查。\n\n实际页面支持外观切换；本样例不宣称后台任务执行成功。",
        truncated: false,
      });
    }
    if (p.endsWith("/backlinks")) return ok({ items: [] });
    if (p.endsWith("/conversations")) return ok([]);
    if (new URL(p, "http://demo").pathname.endsWith("/planner/run"))
      return ok({
        input_contract: "canonical",
        runtime_state: "idle",
        history_replacement_supported: false,
        terminal_timeout: null,
        pending_inputs: [],
        active_input: null,
        native_turn_id: null,
        recovery: null,
        manual_compact_supported: false,
        compact_outcome: null,
        context_usage_invalidated: false,
        card_id: PLANNER,
        worker_session_id: null,
        phase: "idle",
        model: null,
        reasoning_effort: null,
        permission_mode: "never",
        blocked_reason: null,
        pending_queue: [],
        running_turn: null,
        final_reply: null,
      });
    if (p.includes("/harness/items")) return ok(conversationAt(time));
    if (p === "/api/agent-providers/managed")
      return ok([
        {
          availability: {
            provider: "codex",
            status: "ready",
            reason: null,
            checked_at_ms: 1,
            authentication_notice: null,
          },
          capabilities: {
            managed_conversation: true,
            steer: false,
            images: false,
            manual_compact: false,
            history_replacement: false,
            requires_ready_availability: true,
          },
        },
      ]);
    if (p === "/api/agent-providers")
      return ok([
        {
          provider: "codex",
          status: "ready",
          reason: null,
          checked_at_ms: 1,
          authentication_notice: null,
        },
      ]);
    if (p === "/api/track-templates") return ok([]);
    if (p === "/api/settings") return ok({});
    if (
      p === "/api/plugins" ||
      p.includes("/folders") ||
      p.includes("/overlays") ||
      p.includes("/recipes") ||
      p === "/api/track-recipes" ||
      p.includes("/templates")
    )
      return ok([]);
    if (p.includes("/previews")) return ok({ previews: [] });
    if (
      p.startsWith("/api/today/launchpad") ||
      p.startsWith("/api/today/daily")
    )
      return ok(null);
    console.warn("Unscripted read", p);
    return {
      status: 404,
      statusText: "Not Found",
      body: { message: "演示未注册此读取" },
    };
  },
};
// Scene-only framing: retain the native conversation but leave task checks visible above it.
const framing = document.createElement("style");
framing.textContent =
  "[data-nc-panel]{visibility:visible!important}[data-nc-drawer-stack]{inset-block-start:auto!important;block-size:410px!important;max-block-size:54vh!important}[data-nc-drawer]{inset-block-start:auto!important;block-size:410px!important;max-block-size:54vh!important}";
document.head.append(framing);
const registry = createCardRegistry();
bootCards(registry);
const unauthorized = createUnauthorizedChannel({ enqueue: (cb) => cb() });
const router = createAppRouter({
  prepareReport: (report, signal) =>
    runWorkerTask(() => new ReportWorker(), report, signal),
  transport,
  unauthorized,
  client,
  cards: { registry, host: createCardHost(registry) },
  onSignOut: () => {},
});
router.update({
  history: createMemoryHistory({ initialEntries: ["/next/"] }),
});
createRoot(document.getElementById("root")!).render(
  <QueryClientProvider client={client}>
    <ThemeProvider storage={{ getItem: () => null, setItem: () => {} }}>
      <RouterProvider router={router} />
    </ThemeProvider>
  </QueryClientProvider>,
);
const writerNode = document.createElement("div");
writerNode.id = "demo-thought-layer";
writerNode.style.cssText =
  "position:fixed;inset:0;pointer-events:none;z-index:10000";
document.body.append(writerNode);
const writerRoot = createRoot(writerNode);
let lastSignature = "",
  lastConversation = "";
let answerPanel = false,
  answerSent = false;
async function closeConversation() {
  const button = named("Close conversation");
  if (!button) return;
  button.click();
  await settle();
  const deadline = performance.now() + 1500;
  while (named("Close conversation") && performance.now() < deadline)
    await settle();
  if (named("Close conversation")) throw Error("Conversation did not close");
}
async function seek(t: number) {
  const globalTime = t;
  time = Math.max(0, contentTimeAt(t));
  if (globalTime < INTRO || !trackCreated) {
    await introAt(globalTime, {
      get areaCreated() {
        return areaCreated;
      },
      get trackCreated() {
        return trackCreated;
      },
      async reset() {
        document.dispatchEvent(
          new KeyboardEvent("keydown", { key: "Escape", bubbles: true }),
        );
        areaCreated = false;
        trackCreated = false;
        answerPanel = false;
        answerSent = false;
        lastSignature = "";
        lastConversation = "";
        receipts.clear();
        client.clear();
        await router.navigate({ to: "/" });
        await settle();
      },
      async go(path) {
        if (router.state.location.pathname !== "/") {
          await router.navigate({ to: "/" });
          await settle();
        }
      },
    });
    if (globalTime < INTRO && !trackCreated) {
      writerRoot.render(null);
      return { time: globalTime };
    }
  }
  t = time;
  // The post-create intro is also a projection target when rewinding from a completed report.
  if (globalTime < INTRO && trackCreated) {
    answerPanel = false;
    answerSent = false;
    if (!named("Close conversation")) {
      named("Planner")?.click();
      await settle();
    }
    fill(
      document.querySelector<HTMLElement>(
        '[contenteditable="true"][aria-label="Message"]',
      ),
      "",
    );
  }
  // The current frontend keeps the conversation in its own pane state.
  // Drive the real close/open controls rather than inventing a route parameter.
  if (
    globalTime >= INTRO &&
    !answerPanel &&
    !(t >= 7.1 && t < 9.2) &&
    named("Close conversation")
  ) {
    await closeConversation();
  }
  const wantAnswer = t >= 7.1 && t < 9.2;
  if (t < 8.9) answerSent = false;
  if (wantAnswer || (t >= 9.2 && !answerSent)) {
    if (!answerPanel) {
      if (!named("Close conversation")) named("Planner")?.click();
      await settle();
      await settle();
      answerPanel = true;
    }
    const field = document.querySelector<HTMLElement>(
      '[contenteditable="true"][aria-label="Message"]',
    );
    if (!answerSent) {
      fill(field, typed("两种都要。", t, 7.2, 8.5));
      await settle();
    }
    if (t >= 8.9 && !answerSent) {
      fill(field, "两种都要。");
      await settle();
      named("Send")?.click();
      await settle();
    }
  }
  if (!wantAnswer && answerPanel) {
    await closeConversation();
    answerPanel = false;
  }
  const next = trackDetailSchema.parse(detailAt(t)),
    signature = JSON.stringify(next.cards[1].payload);
  if (signature !== lastSignature) {
    flushSync(() => {
      client.setQueryData(queryKeys.trackDetail(TRACK), next);
      client.setQueryData(queryKeys.trackReport(TRACK), verdictsAt(t));
    });
    lastSignature = signature;
  }
  const conversationSignature = JSON.stringify(conversationAt(t));
  if (conversationSignature !== lastConversation) {
    await client.invalidateQueries({
      queryKey: queryKeys.harnessItems(PLANNER),
    });
    lastConversation = conversationSignature;
  }
  await settle();
  const reportRegion = document.querySelector('[aria-label="Track report"]');
  const reportDeadline = performance.now() + 3000;
  while (
    reportRegion?.getAttribute("aria-busy") === "true" &&
    performance.now() < reportDeadline
  )
    await settle();
  if (reportRegion?.getAttribute("aria-busy") === "true")
    throw Error("Native report preparation did not settle");
  if (t >= 4.5) {
    const progress = [
      ...document.querySelectorAll<HTMLElement>("[data-nc-panel] summary"),
    ].find((e) => e.textContent?.trim() === "Progress");
    if (progress && !(progress.closest("details") as HTMLDetailsElement).open) {
      progress.click();
      await settle();
    }
  }
  const wantFile = t >= 16 && t < 19;
  const fileButton = [
    ...document.querySelectorAll<HTMLButtonElement>("[data-nc-report] button"),
  ].find((e) => e.textContent?.trim() === "使用说明");
  if (
    wantFile &&
    !document.querySelector("[data-nc-link-preview]") &&
    fileButton
  ) {
    // Use the native hover admission and delay, without opening the file viewer.
    fileButton.dispatchEvent(
      new PointerEvent("pointerover", { bubbles: true, pointerType: "mouse" }),
    );
    await new Promise((r) => setTimeout(r, 320));
    await settle();
  }
  if (!wantFile && document.querySelector("[data-nc-link-preview]")) {
    document.dispatchEvent(
      new KeyboardEvent("keydown", { key: "Escape", bubbles: true }),
    );
    await settle();
    if (document.activeElement instanceof HTMLElement)
      document.activeElement.blur();
  }
  const preview = document.querySelector<HTMLIFrameElement>(
    'iframe[title="小店页面"]',
  );
  if (preview && t >= 20) {
    const ancestors = [];
    let e = preview.parentElement;
    while (e) {
      if (
        e.scrollHeight > e.clientHeight + 30 &&
        ["auto", "scroll"].includes(getComputedStyle(e).overflowY)
      )
        ancestors.push(e);
      e = e.parentElement;
    }
    const container = ancestors.at(-1);
    if (container) {
      const top =
        preview.getBoundingClientRect().top -
        container.getBoundingClientRect().top +
        container.scrollTop -
        65;
      const u = Math.max(0, Math.min(1, (t - 20) / 2));
      container.scrollTop = top * u * u * u * (10 + u * (-15 + 6 * u));
    }
    // The native preview has an opaque sandbox origin. Target its exact window,
    // send only a non-secret enum, and keep the child sender checks intact.
    preview.contentWindow?.postMessage(
      {
        channel: "neige-demo-theme",
        mode: t >= 28 ? "light" : t >= 24 ? "dark" : "system",
      },
      "*",
    );
  } else if (t < 20) {
    document.querySelectorAll("section").forEach((e) => {
      if (e.scrollHeight > e.clientHeight) e.scrollTop = 0;
    });
  }
  const prose = document.querySelector("[data-nc-report-reading]");
  let cursor: DOMRect | null = null;
  if (prose) {
    const walker = document.createTreeWalker(prose, NodeFilter.SHOW_TEXT);
    let node: Node | null,
      last: Node | null = null;
    while ((node = walker.nextNode()))
      if (node.textContent?.trim()) last = node;
    if (last) {
      const range = document.createRange();
      range.setStart(last, Math.max(0, (last.textContent?.length ?? 1) - 1));
      range.setEnd(last, last.textContent?.length ?? 0);
      cursor = range.getBoundingClientRect();
    }
  }
  flushSync(() =>
    writerRoot.render(
      <>
        <RelayCompanion
          time={globalTime}
          dock={prose?.getBoundingClientRect() ?? null}
          heading={
            prose?.querySelector("h3:last-of-type")?.getBoundingClientRect() ??
            null
          }
        />
        {!relayAt(globalTime).visible && (
          <ThoughtCompanion
            time={t}
            cursor={cursor}
            dock={
              prose
                ?.querySelector("h3:last-of-type")
                ?.getBoundingClientRect() ?? null
            }
          />
        )}
        <DeliveryCompanion
          time={t}
          link={fileButton?.getBoundingClientRect() ?? null}
          dock={prose?.getBoundingClientRect() ?? null}
        />
      </>,
    ),
  );
  return { time, characters: reportAt(t).markdown.length, fileOpen: wantFile };
}
(window as any).__neigeDemo = {
  seek,
  get requests() {
    return requests;
  },
  get state() {
    return reportAt(time);
  },
  router,
};
