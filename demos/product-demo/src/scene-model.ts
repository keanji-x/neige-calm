import type { Knot, Point } from "./model.ts";
export type Participant = (Point & {
  id: string;
  scale: number;
  visibility: Knot[];
}) &
  (
    | {
        kind: "person";
        facing: "front" | "back";
        shirt: string;
        hair: string;
        skin: string;
      }
    | { kind: "robot"; mark: string }
  );
export type SceneSpec = {
  id: string;
  title: string;
  duration: number;
  fps: number;
  width: number;
  height: number;
  environment: "study" | "desk" | "lounge";
  participants: Participant[];
  camera: { zoom: Knot[]; focusX: Knot[]; focusY: Knot[] };
  dialogue: {
    id: string;
    start: number;
    end: number;
    speaker: string;
    text: string;
    box: Point & { width: number; height: number };
  }[];
  gestures: {
    participant: string;
    kind: "answer" | "point" | "listen";
    start: number;
    end: number;
  }[];
};
export const roomLayout = {
  width: 2400,
  height: 1600,
  screen: { x: 780, y: 260, width: 1056, height: 660 },
  desk: { x: 300, y: 980, width: 1880, height: 90 },
};
