/** jsdom has no native animations. Model only the platform control protocol; browsers verify rendering. */
if (typeof Element !== 'undefined' && typeof Element.prototype.animate !== 'function') {
  Object.defineProperty(Element.prototype, 'animate', {
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
