import type { Participant, SceneSpec } from "./scene-model.ts";
import { roomLayout } from "./scene-model.ts";
import { sample, ease } from "./motion.ts";
export function gesture(
  s: SceneSpec,
  id: string,
  kind: SceneSpec["gestures"][number]["kind"],
  t: number,
) {
  let weight = 0;
  for (const g of s.gestures)
    if (g.participant === id && g.kind === kind)
      weight = Math.max(
        weight,
        ease((t - g.start) / 0.35) * (1 - ease((t - g.end + 0.35) / 0.35)),
      );
  return weight;
}
export function sceneCamera(s: SceneSpec, t: number) {
  const fit = Math.min(
    s.width / roomLayout.width,
    s.height / roomLayout.height,
  );
  return {
    scale: fit * sample(s.camera.zoom, t),
    x: sample(s.camera.focusX, t),
    y: sample(s.camera.focusY, t),
  };
}
export function head(p: Participant) {
  return {
    x: p.x + (p.kind === "person" ? 80 : 48) * p.scale,
    y: p.y + (p.kind === "person" ? 62 : 46) * p.scale,
  };
}
export function project(s: SceneSpec, t: number, p: { x: number; y: number }) {
  const c = sceneCamera(s, t);
  return {
    x: s.width / 2 + (p.x - c.x) * c.scale,
    y: s.height / 2 + (p.y - c.y) * c.scale,
  };
}
export function validateStage(s: SceneSpec) {
  const ids = new Set(s.participants.map((p) => p.id));
  if (ids.size !== s.participants.length)
    throw Error("Duplicate scene participant");
  if (!["study", "desk", "lounge"].includes(s.environment))
    throw Error("Unknown environment");
  if (s.duration <= 0 || s.fps < 30) throw Error("Invalid scene timing");
  const tracks = [s.camera.zoom, s.camera.focusX, s.camera.focusY];
  for (const keys of tracks) {
    if (
      keys.length < 2 ||
      keys[0].at > 0 ||
      keys[keys.length - 1].at < s.duration
    )
      throw Error("Incomplete scene timeline");
    for (let i = 1; i < keys.length; i++)
      if (keys[i].at <= keys[i - 1].at) throw Error("Scene keys must increase");
  }
  for (const g of s.gestures) {
    const actor = s.participants.find((p) => p.id === g.participant);
    if (!actor) throw Error("Gesture has no participant");
    if (
      (g.kind === "answer" && actor.kind !== "person") ||
      (g.kind === "point" && actor.kind !== "robot")
    )
      throw Error("Gesture does not match actor");
  }
  for (const g of s.gestures)
    if (g.start < 0 || g.end <= g.start || g.end > s.duration)
      throw Error("Invalid gesture timing");
  for (const actor of s.participants) {
    if (
      actor.visibility.length < 2 ||
      actor.visibility[0].at > 0 ||
      actor.visibility[actor.visibility.length - 1].at < s.duration
    )
      throw Error("Incomplete participant visibility");
  }
  for (const d of s.dialogue) {
    if (!ids.has(d.speaker)) throw Error("Dialogue has no physical speaker");
    if (d.start < 0 || d.end > s.duration || d.end <= d.start)
      throw Error("Invalid dialogue span");
    if (d.text.replace(/\s/g, "").length / (d.end - d.start) > 11)
      throw Error("Scene dialogue too short to read");
    for (let t = d.start; t < d.end; t += 1 / s.fps) {
      const actor = s.participants.find((p) => p.id === d.speaker)!;
      if (sample(actor.visibility, t) < 0.5)
        throw Error("Speaking participant is hidden");
      const face = project(s, t, head(actor)),
        radius = 40 * actor.scale * sceneCamera(s, t).scale;
      if (
        face.x < radius ||
        face.x > s.width - radius ||
        face.y < radius ||
        face.y > s.height - radius
      )
        throw Error("Camera loses physical speaker: " + d.id);
      const a = project(s, t, d.box),
        b = project(s, t, {
          x: d.box.x + d.box.width,
          y: d.box.y + d.box.height,
        });
      if (a.x < 8 || a.y < 8 || b.x > s.width - 8 || b.y > s.height - 8)
        throw Error("Camera loses speaking participant dialogue: " + d.id);
    }
  }
  return true;
}
