// Drive the native controls. Every mutation is handled only by our in-memory transport.
export const INTRO = 12;
export const AREA_NAME = "像素小店 · 演示项目";
export const REQUEST =
  "给像素小店加上暗色模式，做好后让我直接试用，再附一份使用说明。";
export const buttons = () => [
  ...document.querySelectorAll<HTMLButtonElement>("button"),
];
export const named = (label: string) =>
  buttons().find(
    (e) =>
      e.getAttribute("aria-label") === label || e.textContent?.trim() === label,
  );
export async function settle() {
  await Promise.resolve();
  await new Promise<void>((r) => requestAnimationFrame(() => r()));
}
export function fill(field: HTMLElement | null, value: string) {
  if (!field) return;
  if (
    field instanceof HTMLInputElement ||
    field instanceof HTMLTextAreaElement
  ) {
    const setter = Object.getOwnPropertyDescriptor(
      field instanceof HTMLInputElement
        ? HTMLInputElement.prototype
        : HTMLTextAreaElement.prototype,
      "value",
    )!.set!;
    setter.call(field, value);
  } else {
    field.textContent = value;
  }
  field.focus({ preventScroll: true });
  field.dispatchEvent(
    new InputEvent("input", {
      bubbles: true,
      inputType: "insertText",
      data: value,
    }),
  );
}
export function typed(text: string, t: number, start: number, end: number) {
  if (t < start) return "";
  // One character establishes the caret, then natural phrase-sized bursts.
  const p = Math.min(1, Math.max(0, (t - start) / (end - start)));
  return text.slice(
    0,
    Math.min(text.length, p < 0.12 ? 1 : (Math.ceil(p * 6) / 6) * text.length),
  );
}
export type IntroHost = {
  areaCreated: boolean;
  trackCreated: boolean;
  reset: () => Promise<void>;
  go: (path: string) => Promise<void>;
};
export async function introAt(t: number, host: IntroHost) {
  const wantArea = t >= 3.3,
    wantTrack = t >= 9;
  if ((host.areaCreated && !wantArea) || (host.trackCreated && !wantTrack))
    await host.reset();
  if (t < 0.8) {
    document.dispatchEvent(
      new KeyboardEvent("keydown", { key: "Escape", bubbles: true }),
    );
    await host.go("/next/");
    return;
  }
  if (!host.areaCreated) {
    if (!document.querySelector('[role="dialog"] input'))
      named("New area")?.click();
    await settle();
    const field = document.querySelector<HTMLInputElement>(
      '[role="dialog"] input',
    );
    fill(field, typed(AREA_NAME, t, 1.1, 2.7));
    await settle();
    if (!wantArea) return;
    fill(field, AREA_NAME);
    await settle();
    named("Create area")?.click();
    await settle();
    await settle();
  }
  if (t < 4.2) return;
  if (!host.trackCreated) {
    if (!document.querySelector("[data-nc-new-track-message]"))
      named(`New track in ${AREA_NAME}`)?.click();
    await settle();
    await settle();
    const field = document.querySelector<HTMLElement>(
      '[data-nc-new-track-message] [contenteditable="true"]',
    );
    fill(field, typed(REQUEST, t, 5, 7.8));
    await settle();
    if (!wantTrack) return;
    fill(field, REQUEST);
    await settle();
    named("Create track")?.click();
    await settle();
    await settle();
  }
}
export function transcriptAt(t: number) {
  const texts = [
    REQUEST,
    ...(t >= 4.5 ? ["跟随系统，还是手动切换？"] : []),
    ...(t >= 8.9 ? ["两种都要。"] : []),
  ];
  return texts.map((text, i) => ({
    id: i + 1,
    worker_session_id: "demo-only",
    card_id: "44444444444444448444444444444444",
    track_id: "11111111111141118111111111111111",
    thread_id: "demo-story",
    turn_id: `demo-turn-${i}`,
    item_uuid: null,
    item_type: i === 1 ? "agentMessage" : "userMessage",
    method: "item/completed",
    params: JSON.stringify({
      item: i === 1 ? { text } : { content: [{ type: "text", text }] },
    }),
    created_at_ms: 1792166400000 + i * 1000,
    turn_error_text: null,
  }));
}
export function conversationAt(t: number) {
  return {
    contract: "conversation_v1",
    source: "presentation",
    next_cursor: null,
    items: transcriptAt(t).map((row) => ({
      cursor: {
        contract: "conversation_v1",
        source: "presentation",
        card_id: row.card_id,
        position: row.id,
      },
      source:
        row.item_type === "agentMessage"
          ? { kind: "native", item: row }
          : {
              kind: "human",
              message_id: `demo-message-${row.id}`,
              revision: 1,
              author: { user_id: "demo-viewer", display_name: "你" },
              text: JSON.parse(row.params).item.content[0].text,
              attachments: [],
              created_at_ms: row.created_at_ms,
            },
    })),
  };
}
