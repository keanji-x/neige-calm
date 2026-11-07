import fs from "node:fs";
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
const manifest = JSON.parse(
  fs.readFileSync(new URL("../videos/manifest.json", import.meta.url), "utf8"),
);
for (const video of manifest.videos) {
  const file = new URL("../videos/" + video.file, import.meta.url);
  if (
    createHash("sha256").update(fs.readFileSync(file)).digest("hex") !==
    video.sha256
  )
    throw Error("Video checksum mismatch: " + video.file);
  const probe = spawnSync(
    "ffprobe",
    [
      "-v",
      "error",
      "-select_streams",
      "v:0",
      "-show_entries",
      "stream=width,height,r_frame_rate,nb_frames,duration",
      "-of",
      "json",
      file.pathname,
    ],
    { encoding: "utf8" },
  );
  if (probe.status !== 0) throw Error(probe.stderr || "ffprobe failed");
  const v = JSON.parse(probe.stdout).streams[0];
  if (
    v.width !== 1600 ||
    v.height !== 1000 ||
    v.r_frame_rate !== "30/1" ||
    Number(v.nb_frames) !== video.seconds * 30 ||
    Number(v.duration) !== video.seconds
  )
    throw Error("Video metadata mismatch: " + video.file);
  const decode = spawnSync(
    "ffmpeg",
    ["-v", "error", "-threads", "2", "-i", file.pathname, "-f", "null", "-"],
    { encoding: "utf8" },
  );
  if (decode.status !== 0 || decode.stderr.trim())
    throw Error(decode.stderr || "Video decode failed");
  console.log(video.file + ": checksum, dimensions, frame count and decode OK");
}
