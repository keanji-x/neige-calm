import { RobotFigure } from "../src/RobotFigure";
import { thoughtAt } from "./state";
const ease = (v: number) => v * v * v * (10 + v * (-15 + 6 * v));
export function ThoughtCompanion({
  time,
  cursor,
  dock,
}: {
  time: number;
  cursor: DOMRect | null;
  dock: DOMRect | null;
}) {
  const thought = thoughtAt(time);
  if (!thought.visible || !cursor || !dock) return null;
  const left = Math.min(1110, Math.max(320, dock.right - 105));
  const top = Math.min(innerHeight - 150, Math.max(90, dock.top + 30));
  const f = ease(thought.flight),
    enter = Math.min(1, thought.phase / 0.12);
  const startX = left - 205,
    startY = top - 71;
  const x = startX + (cursor.right - startX - 15) * f;
  const y =
    startY + (cursor.top - startY - 12) * f - Math.sin(f * Math.PI) * 36;
  return (
    <>
      <span
        data-demo-caret="true"
        style={{
          position: "fixed",
          left: cursor.right + 2,
          top: cursor.top,
          height: Math.max(16, cursor.height),
          width: 3,
          background: "#9a7ac9",
          opacity: 0.6,
        }}
      />
      <div
        data-demo-thinker="true"
        style={{
          position: "fixed",
          left,
          top,
          width: 100,
          height: 135,
          transform: "scale(.9)",
          transformOrigin: "0 0",
          pointerEvents: "none",
        }}
      >
        <RobotFigure
          mark="/codex-pixel.svg"
          time={time}
          point={thought.flight * 0.55}
          pose={{
            x: 0,
            y: 0,
            moving: false,
            lean: (1 - f) * -5,
            reach: "side",
            celebrate: false,
          }}
        />
        {!thought.landed &&
          thought.flight === 0 &&
          [0, 1, 2].map((i) => (
            <span
              key={i}
              style={{
                position: "absolute",
                left: 8 - i * 9,
                top: -6 - i * 12,
                width: 5 + i * 3,
                height: 5 + i * 3,
                borderRadius: "50%",
                background: "#fff8e8",
                border: "2px solid #8e79aa",
                opacity: enter,
              }}
            />
          ))}
      </div>
      {!thought.landed && (
        <div
          data-demo-thought="true"
          data-flight={thought.flight.toFixed(3)}
          style={{
            position: "fixed",
            left: x,
            top: y,
            maxWidth: 270,
            padding: "10px 17px",
            font: "500 16px/1.5 system-ui",
            color: "#5c4777",
            background: "#fff8e8",
            border: "2px solid #bba6d1",
            borderRadius: 19,
            boxShadow: "0 4px 0 #d8cce533",
            opacity: enter * (1 - 0.6 * f),
            transform: `scale(${(0.86 + 0.14 * enter) * (1 - 0.3 * f)})`,
            transformOrigin: "0 50%",
            whiteSpace: "nowrap",
            pointerEvents: "none",
          }}
        >
          {thought.text}
        </div>
      )}
      {thought.landed && (
        <span
          data-demo-landing="true"
          style={{
            position: "fixed",
            left: cursor.right - 5,
            top: cursor.top - 5,
            width: 20,
            height: 30,
            borderRadius: 12,
            border: "2px solid #bba6d1",
            opacity: Math.max(0, 1 - (thought.phase - 0.9) * 10),
            transform: `scale(${1 + (thought.phase - 0.9) * 6})`,
            pointerEvents: "none",
          }}
        />
      )}
    </>
  );
}
