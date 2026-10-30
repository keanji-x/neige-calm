import { createSeekQueue } from "./seek-queue";
import { createRoot } from "react-dom/client";
import { flushSync } from "react-dom";
import { useEffect, useRef, useState } from "react";
import { SceneStage } from "../src/SceneStage";
import { validateStage } from "../src/scene-motion.ts";
import data from "./spec.json";
import type { SceneSpec } from "../src/scene-model.ts";
const spec = data as SceneSpec & {
  frontend: { entry: string; viewport: { width: number; height: number } };
};
validateStage(spec);
function App() {
  const [time, setTime] = useState(0),
    [ready, setReady] = useState(false),
    [playing, setPlaying] = useState(false);
  const frame = useRef<HTMLIFrameElement>(null),
    clock = useRef(0),
    run = useRef(false),
    busy = useRef(false);
  const stageScale = Math.min(
    innerWidth / spec.width,
    (innerHeight - 70) / spec.height,
  );
  const lane = useRef<ReturnType<typeof createSeekQueue> | null>(null);
  if (lane.current === null)
    lane.current = createSeekQueue(
      async (target) => {
        const bridge = frame.current?.contentWindow?.__neigeDemo;
        if (!bridge) throw Error("Demo is not ready");
        await bridge.seek(target);
      },
      (target) => {
        // Playback animates the outer camera continuously; manual seeks publish only their own target.
        if (!run.current) {
          clock.current = target;
          flushSync(() => setTime(target));
        }
      },
    );
  async function seek(t: number) {
    run.current = false;
    setPlaying(false);
    const target = Math.max(0, Math.min(spec.duration, t));
    clock.current = target;
    flushSync(() => setTime(target));
    await lane.current!.seek(target);
    return clock.current;
  }
  useEffect(() => {
    const timer = setInterval(() => {
      if (frame.current?.contentWindow?.__neigeDemo) {
        setReady(true);
        clearInterval(timer);
      }
    }, 100);
    return () => clearInterval(timer);
  }, []);
  useEffect(() => {
    let id = 0,
      last = performance.now();
    function tick(now: number) {
      const dt = Math.min(0.08, (now - last) / 1000);
      last = now;
      if (run.current) {
        clock.current = Math.min(spec.duration, clock.current + dt);
        setTime(clock.current);
        if (!busy.current && frame.current?.contentWindow?.__neigeDemo) {
          busy.current = true;
          void lane.current!.seek(clock.current).finally(() => {
            busy.current = false;
          });
        }
        if (clock.current >= spec.duration) {
          run.current = false;
          setPlaying(false);
          void lane.current!.seek(spec.duration);
        }
      }
      id = requestAnimationFrame(tick);
    }
    id = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(id);
  }, []);
  useEffect(() => {
    window.demoDirector = {
      seek,
      pause() {
        return seek(clock.current);
      },
      settled() {
        return lane.current!.settled();
      },
      play() {
        run.current = true;
        setPlaying(true);
      },
      get time() {
        return clock.current;
      },
      get ready() {
        return ready;
      },
      exportMode() {
        document.body.dataset.export = "1";
      },
    };
    return () => {
      delete window.demoDirector;
    };
  }, [ready]);
  return (
    <>
      <style>{`*{box-sizing:border-box}body[data-export="1"] .operator{display:none}body[data-export="1"] .stage-wrap{left:0!important;top:0!important;transform:scale(0.8333333333333334)!important}.operator{position:fixed;bottom:0;left:0;right:0;height:64px;background:#272330;color:#f8f3e8;display:flex;align-items:center;gap:18px;padding:12px 24px;font:14px system-ui}.operator button{padding:8px 16px;border:0;border-radius:6px;background:#c9bfeb;color:#443e61}.operator input{flex:1}.badge{opacity:.65}`}</style>
      <div
        className="stage-wrap"
        style={{
          position: "absolute",
          left: (innerWidth - spec.width * stageScale) / 2,
          top: 10,
          width: spec.width,
          height: spec.height,
          transform: `scale(${stageScale})`,
          transformOrigin: "0 0",
        }}
      >
        <SceneStage
          scene={spec}
          time={time}
          viewport={spec.frontend.viewport}
          asset={(name) => "/" + name}
          logo="/neige.svg"
          screenContent={
            <iframe
              ref={frame}
              title="Neige 真实前端演示"
              src={spec.frontend.entry}
              style={{ border: 0, width: 1600, height: 1000, display: "block" }}
            />
          }
        />
      </div>
      <div className="operator">
        <button
          disabled={!ready}
          onClick={() => {
            if (run.current) void seek(clock.current);
            else {
              run.current = true;
              setPlaying(true);
            }
          }}
        >
          {!ready ? "载入真实前端…" : playing ? "暂停" : "播放"}
        </button>
        <button
          disabled={!ready}
          onClick={() => {
            run.current = false;
            setPlaying(false);
            void seek(0);
          }}
        >
          重新开始
        </button>
        <input
          aria-label="演示时间"
          type="range"
          min="0"
          max={spec.duration}
          step="0.05"
          value={time}
          onChange={(e) => {
            run.current = false;
            setPlaying(false);
            void seek(Number(e.target.value));
          }}
        />
        <span>
          {time.toFixed(1)} / {spec.duration}s
        </span>
        <span className="badge">真实前端组件 · 沙盒数据 · 可直接操作</span>
      </div>
    </>
  );
}
createRoot(document.getElementById("root")!).render(<App />);
