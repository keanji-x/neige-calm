import { RobotFigure } from "../src/RobotFigure";
const smooth = (v: number) => {
  v = Math.max(0, Math.min(1, v));
  return v * v * v * (10 + v * (-15 + 6 * v));
};
export function DeliveryCompanion({
  time: t,
  link,
  dock,
}: {
  time: number;
  link: DOMRect | null;
  dock: DOMRect | null;
}) {
  if (!link || !dock || t < 15 || t >= 20) return null;
  const u = smooth((t - 15) / 0.9),
    x = dock.right - 105 + (link.left - 22 - (dock.right - 105)) * u;
  const y =
    dock.top +
    30 +
    (link.top - 85 - (dock.top + 30)) * u -
    Math.sin(u * Math.PI) * 90;
  const send = smooth((t - 19) / 1),
    sendActive = t >= 19;
  return (
    <>
      <div
        data-demo-delivery="true"
        style={{
          position: "fixed",
          left: x,
          top: y,
          width: 95,
          height: 120,
          transform: "scale(.72)",
          transformOrigin: "0 0",
          pointerEvents: "none",
        }}
      >
        <RobotFigure
          mark="/codex-pixel.svg"
          time={t}
          point={sendActive ? send * 0.8 : 0}
          pose={{
            x: 0,
            y: 0,
            moving: u > 0 && u < 1,
            lean: Math.sin(u * Math.PI) * -10,
            reach: "side",
            celebrate: false,
          }}
        />
      </div>
      {sendActive && (
        <div
          data-demo-preview-parcel="true"
          style={{
            position: "fixed",
            left: x + (dock.left + 35 - x) * send,
            top:
              y -
              65 +
              (Math.min(innerHeight - 270, link.bottom + 80) - (y - 65)) *
                send -
              Math.sin(send * Math.PI) * 60,
            width: 280,
            height: 190,
            background: "#fff8e8",
            border: "3px solid #9b83bb",
            borderRadius: 12,
            padding: 8,
            boxShadow: "0 10px 24px #443e6122",
            transform: `rotate(${-9 * (1 - send)}deg) scale(${0.72 + 0.28 * send})`,
            opacity: Math.min(1, (t - 19) / 0.12),
            pointerEvents: "none",
            overflow: "hidden",
          }}
        >
          <div
            style={{
              font: "600 14px system-ui",
              color: "#62537e",
              marginBottom: 7,
            }}
          >
            像素小店 · 可以直接试用
          </div>
          <div style={{ height: 145, overflow: "hidden", borderRadius: 5 }}>
            <iframe
              title="送达中的小店预览"
              src="/sample/index.html"
              sandbox="allow-scripts"
              style={{
                width: 1000,
                height: 600,
                border: 0,
                transform: "scale(.26)",
                transformOrigin: "0 0",
              }}
            />
          </div>
        </div>
      )}
    </>
  );
}
