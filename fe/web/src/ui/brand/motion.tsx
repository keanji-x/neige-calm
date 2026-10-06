import { executionFrames, foldCenter, pointsOf, snowFold } from './motion-geometry.ts';
import styles from './motion.module.css';

// Native SVG timing stays with its decorative geometry, shared by every track.
const LOOP = Object.freeze({ dur: '5.6s', repeatCount: 'indefinite' } as const);
const SPLINE = '.42 0 .58 1';

function splines(times: string): string {
  return Array.from({ length: times.split(';').length - 1 }, () => SPLINE).join(';');
}

export type NeigeMotionKind = 'thinking' | 'execution' | 'creation';

/** Decorative, theme-owned vector motion. The owning surface supplies the status name. */
export function NeigeMotion({ kind, className = '' }: Readonly<{ kind: NeigeMotionKind; className?: string }>) {
  return (
    <svg className={`${styles.motion} ${className}`} viewBox="0 0 384 384" aria-hidden="true" focusable="false"
      fill="none" stroke="currentColor" strokeWidth="34" strokeLinecap="round" strokeLinejoin="round" data-nc-motion={kind}>
      <g className={styles.still}>
        {kind === 'execution' ? <><polyline points="108,128 172,192 108,256" /><path d="M204 256H276" /></>
          : [0, 1, 2].map(index => <polyline key={index} points={pointsOf(snowFold(index))} />)}
      </g>
      <g className={styles.animated}>
        {kind === 'thinking' ? <Thinking /> : kind === 'creation' ? <Creation /> : <Execution />}
      </g>
    </svg>
  );
}

function Tween({ attribute, values, times }: Readonly<{ attribute: string; values: string; times: string }>) {
  return <animate attributeName={attribute} values={values} keyTimes={times} {...LOOP} calcMode="spline"
    keySplines={splines(times)} />;
}

function Turn({ values, times }: Readonly<{ values: string; times: string }>) {
  return <animateTransform attributeName="transform" type="rotate" values={values} keyTimes={times} {...LOOP}
    calcMode="spline" keySplines={splines(times)} />;
}

function Thinking() {
  return <g>
    <Turn values="-120 192 192;0 192 192;0 192 192;-120 192 192" times="0;.18;.82;1" />
    {[0, 1, 2].map(index => {
      const points = snowFold(index);
      const [x, y] = foldCenter(points);
      return <g key={index} transform={`translate(${x} ${y})`}>
        <g>
          <animateTransform attributeName="transform" type="scale" values=".03;.03;1;1;.03;.03" keyTimes="0;.18;.44;.56;.82;1"
            {...LOOP} calcMode="spline" keySplines={splines('0;.18;.44;.56;.82;1')} />
          <polyline points={pointsOf(points.map(([px, py]) => [px - x, py - y]))}>
            <Tween attribute="opacity" values="0;0;1;1;0;0" times="0;.18;.28;.72;.82;1" />
          </polyline>
        </g>
        <circle r="17" fill="currentColor" stroke="none">
          <Tween attribute="opacity" values="1;1;0;0;1;1" times="0;.22;.38;.62;.78;1" />
          <animate attributeName="r" values="17;17;4.25;4.25;17;17" keyTimes="0;.22;.38;.62;.78;1" {...LOOP} />
        </circle>
      </g>;
    })}
  </g>;
}

function Creation() {
  const times = '0;.32;.48;.56;.72;1';
  return <g>
    {/* Advance the outer turn during fold closure so each fold keeps turning clockwise. */}
    <Turn values="0 192 192;0 192 192;120 192 192;120 192 192;300 192 192;360 192 192;360 192 192" times="0;.12;.32;.56;.72;.92;1" />
    {[0, 1, 2].map(index => {
      const points = snowFold(0);
      const [x, y] = points[1];
      return <g key={index} transform={`rotate(${index * 120} 192 192)`}>
        <g>
          <animateTransform attributeName="transform" type="translate" values={`${x} 52;${x} 52;${x} ${y};${x} ${y};${x} 52;${x} 52`}
            keyTimes={times} {...LOOP} calcMode="spline" keySplines={splines(times)} />
          <g>
            <Turn values="-150;-150;0;0;-150;-150" times={times} />
            <polyline points={pointsOf(points.map(([px, py]) => [px - x, py - y]))} />
          </g>
        </g>
      </g>;
    })}
  </g>;
}

function Execution() {
  return <>
    {[0, 1, 2].map(index => {
      const frames = executionFrames(index);
      return <polyline key={index} points="108,128 172,192 108,256">
        <animate attributeName="points" values={frames.values} keyTimes={frames.times} {...LOOP} calcMode="linear" />
      </polyline>;
    })}
    <line x1="204" y1="256" x2="276" y2="256">
      <Tween attribute="x2" values="276;276;204;204;276;276" times="0;.18;.323;.677;.82;1" />
      <Tween attribute="opacity" values="1;1;0;0;1;1" times="0;.18;.323;.677;.82;1" />
    </line>
  </>;
}
