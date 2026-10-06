/** jsdom has no native animations. Model only the platform control protocol; browsers verify rendering. */
if (typeof HTMLElement !== 'undefined' && typeof HTMLElement.prototype.animate !== 'function') {
  if (typeof document.timeline === 'undefined') {
    Object.defineProperty(document, 'timeline', { configurable: true, value: Object.freeze({ currentTime: 0 }) });
  }
  Object.defineProperty(HTMLElement.prototype, 'animate', {
    configurable: true,
    value: function () {
      let resolve: (value: Animation) => void;
      let reject: (reason: DOMException) => void;
      const finished = new Promise<Animation>((yes, no) => { resolve = yes; reject = no; });
      const animation = {
        currentTime: 0,
        startTime: null,
        playState: 'running',
        finished,
        cancel() { this.playState = 'idle'; reject(new DOMException('Animation cancelled', 'AbortError')); },
        finish() { this.playState = 'finished'; resolve(this as unknown as Animation); },
      };
      return animation;
    },
  });
}
