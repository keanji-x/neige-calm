import { RobotFigure } from "../src/RobotFigure";
import { relayAt, flight, easeRelay } from "./relay-model";
export function RelayCompanion({
  time: t,
  dock,
  heading,
}: {
  time: number;
  dock: DOMRect | null;
  heading: DOMRect | null;
}) {
  const state = relayAt(t);
  if (!state.visible || !dock) return null;
  // Stage both partners in the report's free space; sidebar and Progress stay clear.
  const c = {
    x: dock.right - 110,
    y: Math.min(innerHeight - 240, Math.max(300, dock.top + 350)),
  };
  const settle = easeRelay((t - 28) / 0.4);
  c.x += (Math.min(1110, Math.max(320, dock.right - 105)) - c.x) * settle;
  c.y += ((heading?.top ?? dock.top + 320) + 30 - c.y) * settle;
  const a = { x: dock.left + 55, y: c.y + 10 };
  const start = { x: c.x + 12, y: c.y + 27 },
    end = { x: a.x + 35, y: a.y + 20 };
  const target = { x: dock.left + 35, y: dock.top + 315 };
  let p = start,
    scale = 1,
    rotate = 0;
  if (state.phase === "draw")
    p = { x: start.x, y: start.y + 26 * (1 - easeRelay((t - 21.5) / 0.4)) };
  if (state.phase === "delegate") {
    p = flight(start, end, t - 22, 95);
    rotate = Math.sin((t - 22) * Math.PI) * 12;
  }
  if (["working", "pack"].includes(state.phase)) p = end;
  if (state.phase === "return") {
    p = flight(end, start, t - 25, 105);
    rotate = -Math.sin((t - 25) * Math.PI) * 12;
  }
  if (state.phase === "report") {
    p = flight(start, target, t - 27, 75);
    scale = 1 - 0.45 * state.deposit;
    rotate = 7 * Math.sin((t - 27) * Math.PI);
  }
  const packaged = ["pack", "return", "receive", "report", "landed"].includes(
    state.phase,
  );
  const cardOffset = state.phase === "report" ? 185 * (1 - state.deposit) : 185;
  const showCard = state.phase !== "working" && state.phase !== "landed";
  const alpha = Math.min(1, (t - 21.5) / 0.15, (28.4 - t) / 0.25);
  const bubble =
    t < 23
      ? "Claude，页面和说明交给你。"
      : t < 24.6
        ? "好，我把三种外观一起做好。"
        : t < 26
          ? "做好了，接住！"
          : "放进 Report，直接交给你。";
  const speaker = t >= 23 && t < 26 ? a : c;
  return (
    <div
      data-demo-relay="true"
      data-phase={state.phase}
      data-owner={state.owner}
      style={{
        position: "fixed",
        inset: 0,
        pointerEvents: "none",
        opacity: alpha,
        font: "500 16px/1.45 system-ui",
        color: "#62537e",
      }}
    >
      <div
        style={{
          position: "absolute",
          left: c.x,
          top: c.y,
          transform: "scale(.88)",
          transformOrigin: "0 0",
        }}
      >
        <RobotFigure
          mark="/codex-pixel.svg"
          time={t}
          point={
            state.phase === "delegate"
              ? state.outbound
              : state.phase === "report"
                ? state.deposit
                : 0
          }
          pose={{
            x: 0,
            y: 0,
            moving: false,
            lean:
              state.phase === "delegate"
                ? Math.sin((t - 22) * Math.PI) * -8
                : 0,
            reach: "side",
            celebrate: state.phase === "receive",
          }}
        />
      </div>
      <div
        data-demo-claude="true"
        style={{
          position: "absolute",
          left: a.x - 12,
          top: a.y,
          width: 110,
          height: 100,
          transform: `translateY(${state.phase === "working" ? -Math.abs(Math.sin(t * 7)) * 6 : 0}px) rotate(${state.phase === "return" ? -Math.sin((t - 25) * Math.PI) * 8 : 0}deg)`,
        }}
      >
        <img
          alt="Claude 小螃蟹"
          src="/claude-pixel.svg"
          style={{ width: 105, height: 105, imageRendering: "pixelated" }}
        />
        {state.phase === "working" && (
          <div
            style={{
              position: "absolute",
              top: 91,
              left: 5,
              display: "flex",
              gap: 6,
            }}
          >
            {[0, 1, 2].map((i) => (
              <span
                key={i}
                style={{
                  width: 20,
                  height: 6,
                  background: (t - 23) / 1.6 > i / 3 ? "#d99070" : "#e7d1c0",
                }}
              />
            ))}
          </div>
        )}
      </div>
      <div
        style={{
          position: "absolute",
          left: Math.min(dock.right - 285, Math.max(dock.left, speaker.x - 30)),
          top: speaker.y - 76,
          maxWidth: 300,
          padding: "10px 16px",
          border: "2px solid " + (speaker === a ? "#d99070" : "#b59ccd"),
          borderRadius: 16,
          background: "#fff8e8",
          boxShadow: "0 4px 0 #d8cce533",
        }}
      >
        {bubble}
      </div>
      {["report", "landed"].includes(state.phase) && (
        <div
          style={{
            position: "absolute",
            left: target.x - 10,
            top: target.y - 13,
            padding: "5px 12px",
            border: "2px dashed #b59ccd",
            borderRadius: 8,
            background: "#fff8e8",
            opacity: state.phase === "landed" ? state.flash : 1,
          }}
        >
          Report
        </div>
      )}
      {showCard && (
        <div
          data-demo-relay-card="true"
          data-artifact={packaged}
          style={{
            position: "absolute",
            left: p.x,
            top: p.y,
            width: 190,
            padding: "12px 14px",
            background: packaged ? "#e5f0df" : "#fff8e8",
            border: "3px solid " + (packaged ? "#8ba783" : "#b59ccd"),
            borderRadius: 8,
            boxShadow: "-6px 6px 0 #e5d9ee",
            transform: `translate(${-cardOffset}px,-25px) rotate(${rotate}deg) scale(${scale})`,
            transformOrigin: "0 50%",
          }}
        >
          <div style={{ fontSize: 12, opacity: 0.65, marginBottom: 4 }}>
            {packaged ? "交付产物" : "任务卡"}
          </div>
          <strong style={{ fontSize: 17 }}>
            {packaged ? "小店预览 + 使用说明" : "深浅色切换 + 使用说明"}
          </strong>
        </div>
      )}
      {state.flash > 0 && (
        <svg
          data-demo-relay-flash="true"
          width="1600"
          height="1000"
          style={{ position: "absolute", inset: 0, opacity: state.flash }}
        >
          {[0, 1, 2, 3, 4, 5].map((i) => {
            const center = t < 22 || t >= 26 ? (t >= 28 ? target : start) : end;
            const angle = (i * Math.PI) / 3;
            return (
              <rect
                key={i}
                x={center.x + Math.cos(angle) * (22 + 16 * (1 - state.flash))}
                y={center.y + Math.sin(angle) * (22 + 16 * (1 - state.flash))}
                width="6"
                height="6"
                fill={i % 2 ? "#d99070" : "#b59ccd"}
              />
            );
          })}
        </svg>
      )}
    </div>
  );
}
