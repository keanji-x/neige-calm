/** jsdom has no native animations. Model only the platform control protocol; browsers verify rendering. */
if (typeof Element !== 'undefined' && typeof Element.prototype.animate !== 'function') {
  // Like the existing manual Animation controls, this is a stationary platform
  // sample, not a rendering clock. DOM tests may seek controls; real browser
  // tests own elapsed time, layout, frame delivery and native promise timing.
  const timelines = new WeakMap<Document, Readonly<{ currentTime: number | null }>>();
  Object.defineProperty(Document.prototype, 'timeline', { configurable: true, get(this: Document) {
    let timeline = timelines.get(this);
    if (timeline === undefined) {
      timeline = Object.freeze({ currentTime: this.defaultView === null ? null : 0 });
      timelines.set(this, timeline);
    }
    return timeline;
  } });
  class StubAnimation {
    private time: number | null = 0;
    private start: number | null = null;
    private rate = 1;
    playState = 'running';
    pending = false;
    ready = Promise.resolve(this);
    finished: Promise<Animation>;
    private resolve!: (value: Animation) => void;
    private reject!: (reason: DOMException) => void;
    constructor(readonly timeline: AnimationTimeline) {
      this.finished = new Promise<Animation>((yes, no) => { this.resolve = yes; this.reject = no; });
    }
    get currentTime() { return this.time; }
    set currentTime(value: number | null) { this.time = value; }
    get startTime() { return this.start; }
    set startTime(value: number | null) { this.start = value; }
    get playbackRate() { return this.rate; }
    set playbackRate(value: number) { this.rate = value; }
    play() { this.playState = 'running'; }
    pause() { this.playState = 'paused'; }
    reverse() { this.rate *= -1; this.play(); }
    updatePlaybackRate(value: number) { this.rate = value; }
    cancel() { this.playState = 'idle'; this.reject(new DOMException('Animation cancelled', 'AbortError')); }
    finish() { this.playState = 'finished'; this.resolve(this as unknown as Animation); }
  }
  Object.defineProperty(Element.prototype, 'animate', { configurable: true, value: function (this: Element) {
    return new StubAnimation(this.ownerDocument.timeline);
  } });
}
