export function RobotFigure({
  mark,
  time,
  pose: p,
  point,
}: {
  mark: string;
  time: number;
  pose: {
    x: number;
    y: number;
    moving: boolean;
    lean: number;
    reach: "side" | "down";
    celebrate: boolean;
  };
  point: number;
}) {
  const down = p.reach === "down",
    wave = p.celebrate ? Math.sin(time * 7) * 10 : 0;
  return (
    <div
      style={{
        position: "absolute",
        left: p.x,
        top: p.y + (p.moving ? 0 : Math.sin(time * 2.4) * 0.6),
        width: 96,
        height: 150,
        rotate: `${p.lean}deg`,
        transformOrigin: "50% 80%",
      }}
    >
      <svg
        viewBox="0 0 96 150"
        width="96"
        height="150"
        shapeRendering="crispEdges"
        overflow="visible"
      >
        <g fill="#62537e">
          <rect x="24" y="78" width="12" height="15" />
          <rect x="56" y="78" width="12" height="15" />
          <rect x="17" y="90" width="22" height="8" />
          <rect x="56" y="90" width="23" height="8" />
        </g>
        <path
          d={`M23 55H${10 - point * 40}V${70 - point * 45}H${4 - point * 55}`}
          fill="none"
          stroke="#62537e"
          strokeWidth="8"
        />
        <rect
          x={-2 - point * 55}
          y={64 - point * 45}
          width="12"
          height="12"
          fill="#f0bfa7"
          opacity={point}
        />
        <g transform={`rotate(${wave} 70 46)`}>
          <path
            d={down ? "M70 46H81V118H48V130" : "M70 46H80V57H87"}
            fill="none"
            stroke="#62537e"
            strokeWidth="8"
          />
          <rect
            x={down ? 42 : 82}
            y={down ? 124 : 51}
            width="12"
            height="12"
            fill="#f0bfa7"
          />
        </g>
        <rect x="18" y="14" width="60" height="64" rx="8" fill="#fffaf1" />
        <image href={mark} x="18" y="16" width="60" height="60" />
        <g fill="#eeb18e">
          <rect x="26" y="5" width="43" height="11" />
          <rect x="33" y="0" width="28" height="8" />
          <rect x="22" y="14" width="55" height="6" />
        </g>
      </svg>
    </div>
  );
}
