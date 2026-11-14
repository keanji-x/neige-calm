export const RELAY_START = 21.5;
export const RELAY_DELAY = 6;
export const RELAY_LAND = 28;
export const contentTimeAt = (global: number) =>
  global - 12 - Math.max(0, Math.min(RELAY_DELAY, global - RELAY_START));
export const easeRelay = (value: number) => {
  const u = Math.max(0, Math.min(1, value));
  return u * u * u * (10 + u * (-15 + 6 * u));
};
export type Point = { x: number; y: number };
export function flight(
  from: Point,
  to: Point,
  progress: number,
  height: number,
) {
  const u = easeRelay(progress);
  return {
    x: from.x + (to.x - from.x) * u,
    y: from.y + (to.y - from.y) * u - Math.sin(u * Math.PI) * height,
  };
}
export function relayAt(t: number) {
  const phase =
    t < 22
      ? "draw"
      : t < 23
        ? "delegate"
        : t < 24.6
          ? "working"
          : t < 25
            ? "pack"
            : t < 26
              ? "return"
              : t < 27
                ? "receive"
                : t < 28
                  ? "report"
                  : "landed";
  const owner =
    t < 23 ? "codex" : t < 26 ? "claude" : t < 28 ? "codex" : "report";
  return {
    visible: t >= RELAY_START && t < 28.4,
    phase,
    owner,
    outbound: easeRelay((t - 22) / 1),
    returning: easeRelay((t - 25) / 1),
    deposit: easeRelay((t - 27) / 1),
    flash: Math.max(
      ...[21.7, 23, 24.65, 26, 28].map((at) =>
        Math.max(0, 1 - Math.abs(t - at) / 0.16),
      ),
    ),
  };
}
