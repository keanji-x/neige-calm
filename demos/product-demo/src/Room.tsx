import type { SceneSpec } from "./scene-model.ts";
const ink = "#443e61";
export function RoomBackdrop({ kind }: { kind: SceneSpec["environment"] }) {
  return (
    <svg
      width="2400"
      height="1600"
      viewBox="0 0 2400 1600"
      shapeRendering="crispEdges"
      style={{ position: "absolute", inset: 0 }}
    >
      <rect width="2400" height="1600" fill="#f7eddd" />
      <rect y="1320" width="2400" height="280" fill="#c5d7c3" />
      <path d="M0 1320H2400" stroke="#a5bba5" strokeWidth="12" />
      {[0, 1, 2, 3, 4, 5, 6, 7, 8].map((i) => (
        <path
          key={i}
          d={`M${i * 300} 1320V1600`}
          stroke="#b5cbb4"
          strokeWidth="4"
        />
      ))}
      <rect x="66" y="130" width="456" height="510" fill="#d8c8be" />
      <rect x="92" y="150" width="410" height="466" fill="#d4e5df" />
      <rect
        x="108"
        y="166"
        width="378"
        height="434"
        fill={kind === "lounge" ? "#e5dbee" : "#d5e6ef"}
      />
      <circle cx="386" cy="258" r="60" fill="#f7dca3" />
      <path d="M108 500H240V460H300V520H486V600H108Z" fill="#b8cfbb" />
      <path d="M290 166V600M108 380H486" stroke="#fff8eb" strokeWidth="14" />
      <rect x="78" y="616" width="440" height="26" fill="#c8b3a7" />
      {kind === "study" && (
        <g>
          <rect x="1890" y="160" width="360" height="488" fill="#d5bbac" />
          <rect x="1912" y="180" width="316" height="448" fill="#f1e3d2" />
          {[0, 1, 2].map((row) => (
            <g key={row}>
              {[0, 1, 2, 3, 4, 5, 6].map((i) => (
                <rect
                  key={i}
                  x={1930 + i * 40}
                  y={207 + row * 142 + (i % 3) * 7}
                  width="29"
                  height={92 - (i % 3) * 7}
                  fill={["#8e86bd", "#adc4af", "#dfac91", "#dacb94"][i % 4]}
                />
              ))}
              <rect
                x="1912"
                y={310 + row * 142}
                width="316"
                height="15"
                fill="#b99380"
              />
            </g>
          ))}
        </g>
      )}
      {kind === "desk" && (
        <g>
          <rect
            x="1920"
            y="250"
            width="280"
            height="280"
            fill="#e5dcef"
            stroke="#c1b3ce"
            strokeWidth="12"
          />
          <path
            d="M1990 450V350H2045V410H2110V300"
            fill="none"
            stroke="#8e86bd"
            strokeWidth="22"
          />
        </g>
      )}
      {kind === "lounge" && (
        <g>
          <rect
            x="1830"
            y="260"
            width="420"
            height="320"
            fill="#fff8eb"
            stroke="#c9b6a6"
            strokeWidth="14"
          />
          <circle cx="2040" cy="410" r="100" fill="#c9bfeb" />
          <path d="M1910 510L2040 350L2180 510Z" fill="#a2c6b2" />
        </g>
      )}
      <path
        d="M2100 870V740M2100 800L2030 750M2100 780L2160 700"
        stroke="#648d7c"
        strokeWidth="18"
      />
      <rect x="2024" y="720" width="60" height="45" fill="#9abc9a" />
      <rect x="2120" y="675" width="65" height="55" fill="#a6c6a2" />
      <path d="M2045 850H2155L2140 965H2060Z" fill="#d9a68d" />
      <rect
        x="422"
        y="820"
        width="275"
        height="300"
        rx="42"
        fill={kind === "lounge" ? "#c9bfeb" : "#a8bfb0"}
      />
      <rect x="475" y="1080" width="220" height="56" fill="#8da496" />
      <path
        d="M565 1140V1450M450 1450H690 M470 1285H700"
        stroke={ink}
        strokeWidth="18"
      />
    </svg>
  );
}
export function Desk() {
  return (
    <svg
      width="2400"
      height="1600"
      viewBox="0 0 2400 1600"
      shapeRendering="crispEdges"
      style={{ position: "absolute", inset: 0, pointerEvents: "none" }}
    >
      <rect x="300" y="990" width="1880" height="90" fill="#d9bda5" />
      <rect x="300" y="980" width="1880" height="28" fill="#ecd5bc" />
      <path
        d="M332 1080V1410M2144 1080V1410"
        stroke="#a78678"
        strokeWidth="56"
      />
      <rect x="1020" y="940" width="480" height="28" rx="8" fill="#b8b1c9" />
      <rect x="1040" y="940" width="435" height="12" fill="#e5dfef" />
      <rect x="1660" y="922" width="80" height="60" rx="8" fill="#fff8e8" />
      <path
        d="M1740 934H1760V965H1740"
        fill="none"
        stroke="#fff8e8"
        strokeWidth="12"
      />
      <rect x="350" y="940" width="110" height="35" fill="#c9bfeb" />
      <rect x="365" y="925" width="110" height="22" fill="#a8c4b0" />
    </svg>
  );
}
