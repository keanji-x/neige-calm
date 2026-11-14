import type { ReactNode } from "react";
import type { SceneSpec } from "./scene-model.ts";
import { roomLayout } from "./scene-model.ts";
import { RoomBackdrop, Desk } from "./Room";
import { Person } from "./Person";
import { RobotFigure } from "./RobotFigure";
import { ease, sample } from "./motion.ts";
import { gesture, head, sceneCamera } from "./scene-motion.ts";
export function SceneStage({
  scene: s,
  time: t,
  viewport,
  screenContent,
  logo,
  asset,
}: {
  scene: SceneSpec;
  time: number;
  viewport: { width: number; height: number };
  screenContent: ReactNode;
  logo: string;
  asset: (name: string) => string;
}) {
  const c = sceneCamera(s, t),
    screen = roomLayout.screen;
  const people = s.participants.filter((p) => p.kind === "person"),
    robots = s.participants.filter((p) => p.kind === "robot");
  const renderPerson = (p: (typeof people)[number]) => (
    <div key={p.id} style={{ opacity: sample(p.visibility, t) }}>
      <Person person={p} time={t} answer={gesture(s, p.id, "answer", t)} />
    </div>
  );
  return (
    <div
      style={{
        position: "absolute",
        inset: 0,
        overflow: "hidden",
        background: "#f7eddd",
        fontFamily: "Noto Sans CJK SC,sans-serif",
        color: "#443e61",
      }}
    >
      <div
        style={{
          position: "absolute",
          width: roomLayout.width,
          height: roomLayout.height,
          left: s.width / 2 - c.x * c.scale,
          top: s.height / 2 - c.y * c.scale,
          scale: c.scale,
          transformOrigin: "0 0",
        }}
      >
        <RoomBackdrop kind={s.environment} />
        {people.filter((p) => p.facing === "front").map(renderPerson)}
        <div
          style={{
            position: "absolute",
            left: screen.x - 24,
            top: screen.y - 24,
            width: screen.width + 48,
            height: screen.height + 48,
            background: "#6b607d",
            border: "10px solid #443e61",
            boxSizing: "border-box",
            boxShadow: "12px 15px 0 #c7b7b688",
          }}
        />
        <div
          style={{
            position: "absolute",
            left: screen.x,
            top: screen.y,
            width: screen.width,
            height: screen.height,
            overflow: "hidden",
            background: "#f7f4ef",
          }}
        >
          <div
            style={{
              position: "relative",
              width: viewport.width,
              height: viewport.height,
              scale: screen.width / viewport.width,
              transformOrigin: "0 0",
            }}
          >
            {screenContent}
          </div>
        </div>
        <div
          style={{
            position: "absolute",
            left: 1230,
            top: 944,
            width: 150,
            height: 35,
            background: "#6b607d",
          }}
        />
        <Desk />
        {robots.map((p) => (
          <div
            key={p.id}
            style={{
              position: "absolute",
              left: p.x,
              top: p.y,
              scale: p.scale,
              transformOrigin: "0 0",
              opacity: sample(p.visibility, t),
            }}
          >
            <RobotFigure
              mark={asset(p.mark)}
              time={t}
              point={gesture(s, p.id, "point", t)}
              pose={{
                x: 0,
                y: 0,
                moving: false,
                lean: gesture(s, p.id, "point", t) * -2,
                reach: "side",
                celebrate: false,
              }}
            />
          </div>
        ))}
        {people.filter((p) => p.facing === "back").map(renderPerson)}
        {s.dialogue
          .filter((d) => t >= d.start && t < d.end)
          .map((d) => {
            const actor = s.participants.find((p) => p.id === d.speaker)!;
            const h = head(actor),
              alpha =
                ease((t - d.start) / 0.2) * (1 - ease((t - d.end + 0.2) / 0.2));
            const baseX = Math.max(
              d.box.x + 30,
              Math.min(h.x, d.box.x + d.box.width - 30),
            );
            const bottom = d.box.y + d.box.height,
              fill = actor.kind === "person" ? "#c6dfce" : "#fff8e8";
            return (
              <div key={d.id} style={{ opacity: alpha }}>
                <svg
                  width="2400"
                  height="1600"
                  style={{
                    position: "absolute",
                    inset: 0,
                    pointerEvents: "none",
                  }}
                >
                  <path
                    d={`M${baseX - 13} ${bottom - 2} L${h.x} ${h.y - 52} L${baseX + 13} ${bottom - 2}Z`}
                    fill={fill}
                    stroke="#443e61"
                    strokeWidth="4"
                  />
                </svg>
                <div
                  style={{
                    position: "absolute",
                    left: d.box.x,
                    top: d.box.y,
                    width: d.box.width,
                    height: d.box.height,
                    boxSizing: "border-box",
                    padding: "22px 30px",
                    border: "4px solid #443e61",
                    background: fill,
                    boxShadow: "7px 8px 0 #c9bfeb88",
                    display: "flex",
                    alignItems: "center",
                    fontSize: 30,
                    lineHeight: 1.4,
                    fontWeight: 550,
                    whiteSpace: "pre-line",
                  }}
                >
                  {d.text}
                </div>
              </div>
            );
          })}
      </div>
      <div
        style={{
          position: "absolute",
          left: 58,
          top: 18,
          zIndex: 5,
          background: "#f7eddd",
          padding: "3px 8px",
          borderRadius: 6,
          display: "flex",
          alignItems: "center",
          gap: 12,
          fontSize: 25,
          fontWeight: 700,
        }}
      >
        <img src={logo} style={{ width: 28, height: 28 }} />
        {s.title}
      </div>
    </div>
  );
}
