import type { Participant } from "./scene-model.ts";
export function Person({
  person,
  answer,
  time,
}: {
  person: Extract<Participant, { kind: "person" }>;
  answer: number;
  time: number;
}) {
  const handX = 137 + answer * 10,
    handY = 180 - answer * 73;
  if (person.facing === "back")
    return (
      <div
        style={{
          position: "absolute",
          left: person.x,
          top: person.y,
          width: 160,
          height: 400,
          scale: person.scale,
          transformOrigin: "0 0",
          rotate: `${answer * 1.2}deg`,
        }}
      >
        <svg
          viewBox="0 0 160 400"
          width="160"
          height="400"
          shapeRendering="crispEdges"
        >
          <path d="M42 115H116L149 220V400H10V218Z" fill={person.shirt} />
          <path d="M50 120H110L104 145H56Z" fill="#7e8b9f" />
          <rect x="59" y="90" width="42" height="39" fill={person.skin} />
          <rect
            x="31"
            y="18"
            width="99"
            height="89"
            rx="19"
            fill={person.hair}
          />
          <path d="M35 68H126V109H109V118H54V109H35Z" fill={person.hair} />
          <path d="M45 25H112V38H45Z" fill="#615a6b" />
          <path
            d="M34 156L8 225M124 156L151 225"
            stroke={person.shirt}
            strokeWidth="26"
            fill="none"
          />
        </svg>
      </div>
    );
  return (
    <div
      style={{
        position: "absolute",
        left: person.x,
        top: person.y,
        width: 160,
        height: 400,
        scale: person.scale,
        transformOrigin: "0 0",
        rotate: `${-answer * 1.5}deg`,
      }}
    >
      <svg
        viewBox="0 0 160 400"
        width="160"
        height="400"
        shapeRendering="crispEdges"
        overflow="visible"
      >
        <path d="M46 192V363H64V228H93V363H112V188Z" fill="#625b78" />
        <rect x="38" y="360" width="37" height="12" fill="#443e61" />
        <rect x="91" y="360" width="37" height="12" fill="#443e61" />
        <path d="M45 105H111L125 199H30Z" fill={person.shirt} />
        <rect x="64" y="88" width="31" height="30" fill={person.skin} />
        <g
          transform={`rotate(${Math.sin(time * 2) * 0.35 + answer * 2} 78 80)`}
        >
          <rect
            x="41"
            y="24"
            width="74"
            height="72"
            rx="15"
            fill={person.skin}
          />
          <path
            d="M32 58V20H48V6H99V15H121V48H103V31H52V61Z"
            fill={person.hair}
          />
          <rect x="79" y="53" width="5" height="7" fill="#443e61" />
          <rect x="102" y="53" width="5" height="7" fill="#443e61" />
          <path
            d={answer > 0.2 ? "M84 77H101V72" : "M86 76H99"}
            stroke="#b87968"
            strokeWidth="4"
            fill="none"
          />
        </g>
        <path
          d="M42 128L20 171L47 180"
          stroke={person.shirt}
          strokeWidth="22"
          fill="none"
        />
        <rect x="41" y="172" width="25" height="14" fill={person.skin} />
        <path
          d={`M109 130L127 ${155 - answer * 28}L${handX} ${handY}`}
          stroke={person.shirt}
          strokeWidth="22"
          fill="none"
        />
        <rect
          x={handX - 8}
          y={handY - 8}
          width="24"
          height="20"
          fill={person.skin}
        />
        <g opacity={answer}>
          <rect
            x={handX - 3}
            y={handY - 29}
            width="6"
            height="27"
            fill={person.skin}
          />
          <rect
            x={handX + 10}
            y={handY - 34}
            width="6"
            height="32"
            fill={person.skin}
          />
        </g>
      </svg>
    </div>
  );
}
