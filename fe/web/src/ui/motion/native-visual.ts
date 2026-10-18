/** Visual invalidation for native effects owned by one playback, never a global observer. */
export function createNativeVisual(animations: readonly Animation[], view: Window) {
  let callback: (() => void) | undefined;
  let disposed = false;
  let installed = false;
  let queued: number | null = null;
  let revision = 0;
  let times: readonly (CSSNumberish | null)[] | null = null;
  const stop = () => { if (queued !== null) view.cancelAnimationFrame(queued); queued = null; };
  const publish = () => {
    const next = animations.map(animation => animation.currentTime);
    if (times !== null && next.every((time, index) => time === times![index])) return;
    times = next; callback?.();
  };
  const moving = () => animations.some(animation => animation.playState === 'running' && animation.playbackRate !== 0 && !animation.pending);
  const tick = () => {
    queued = null;
    if (disposed || callback === undefined) return;
    publish();
    if (moving()) arm();
  };
  const arm = () => {
    if (!disposed && callback !== undefined && queued === null) queued = view.requestAnimationFrame(tick);
  };
  const needsFrame = () => times === null || animations.some((animation, index) => animation.currentTime !== times![index]) || moving();
  const changed = () => {
    if (disposed || callback === undefined) return;
    const current = ++revision;
    if (needsFrame()) arm();
    // Pending pause/play/rate operations may resolve after the one-shot sample.
    // Wait on the native ready promise, never poll a paused or pending clock.
    for (const animation of animations) if (animation.pending) void animation.ready.then(() => {
      if (!disposed && callback !== undefined && revision === current) {
        if (needsFrame()) arm();
      }
    }, () => {});
  };
  const cancel = () => { disposed = true; callback = undefined; revision++; stop(); };
  const install = () => {
    if (installed) return;
    // Fresh native instances must be extensible and expose the public WAAPI
    // descriptors. Validate the entire group before defining any own control.
    const definitions: { animation: Animation; name: string; descriptor: PropertyDescriptor }[] = [];
    const controlled = (receiver: Animation, operation: string) => {
      if (!animations.includes(receiver)) return;
      if (operation === 'cancel') cancel(); else changed();
    };
    for (const animation of animations) {
      if (!Object.isExtensible(animation)) throw new TypeError('Native visual controls require extensible Animations');
      const prototype = Object.getPrototypeOf(animation) as Animation;
      for (const name of ['play', 'pause', 'reverse', 'finish', 'cancel', 'updatePlaybackRate'] as const) {
        const method: unknown = Object.getOwnPropertyDescriptor(prototype, name)?.value;
        if (typeof method !== 'function' || Object.hasOwn(animation, name)) throw new TypeError(`Unavailable native Animation control: ${name}`);
        definitions.push({ animation, name, descriptor: { configurable: true, writable: true, value: function (this: Animation, ...args: unknown[]) {
          const result: unknown = Reflect.apply(method, this, args);
          controlled(this, name);
          return result;
        } } });
      }
      for (const name of ['currentTime', 'startTime', 'playbackRate'] as const) {
        const descriptor = Object.getOwnPropertyDescriptor(prototype, name);
        if (typeof descriptor?.get !== 'function' || typeof descriptor.set !== 'function' || Object.hasOwn(animation, name)) throw new TypeError(`Unavailable native Animation control: ${name}`);
        const setter = Reflect.get(descriptor, 'set')!;
        definitions.push({ animation, name, descriptor: { configurable: true, get: Reflect.get(descriptor, 'get'), set: function (this: Animation, value: unknown) {
          Reflect.apply(setter, this, [value]);
          controlled(this, name);
        } } });
      }
    }
    const applied: typeof definitions = [];
    try {
      for (const definition of definitions) {
        Object.defineProperty(definition.animation, definition.name, definition.descriptor);
        applied.push(definition);
      }
    } catch (error) {
      for (const { animation, name } of applied.reverse()) Reflect.deleteProperty(animation, name);
      throw error;
    }
    installed = true;
  };
  return {
    cancel,
    terminal: () => { stop(); if (!disposed && callback !== undefined) { publish(); if (moving()) arm(); } },
    setCallback: (next: (() => void) | undefined) => {
      if (disposed || next === callback) return;
      const observing = callback !== undefined;
      if (next !== undefined) install();
      callback = next;
      if (next !== undefined && observing) return;
      revision++;
      if (next === undefined) { stop(); return; }
      times = null; changed();
    },
  };
}
