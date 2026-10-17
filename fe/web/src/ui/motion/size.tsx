import { Component, createRef, type ReactNode } from 'react';
import { playSpring, type SpringPlayback } from './spring.ts';
import styles from './size.module.css';

type SizeMotionProps = Readonly<{ children: ReactNode; motionKey: string | boolean }>;

/**
 * Only declared mode changes capture geometry, before React changes the DOM.
 * This commit snapshot has no hook equivalent; it also handles an ordinary
 * resize followed immediately by a mode change, before ResizeObserver runs.
 * Ordinary child updates remain natural, without synchronous measurements.
 */
export class SizeMotion extends Component<SizeMotionProps, never, number | null> {
  private hostRef = createRef<HTMLDivElement>();
  private contentRef = createRef<HTMLDivElement>();
  private controls: SpringPlayback | null = null;
  private targetHeight: number | null = null;
  private observer: ResizeObserver | null = null;
  private preference: MediaQueryList | null = null;

  getSnapshotBeforeUpdate(previous: SizeMotionProps): number | null {
    return previous.motionKey === this.props.motionKey ? null : this.hostRef.current?.getBoundingClientRect().height ?? null;
  }

  componentDidUpdate(_previous: SizeMotionProps, _previousState: never, from: number | null) {
    const content = this.contentRef.current;
    if (from !== null && content !== null) this.resize(from, content.getBoundingClientRect().height);
  }

  componentDidMount() {
    const host = this.hostRef.current;
    const content = this.contentRef.current;
    if (host === null || content === null || typeof ResizeObserver === 'undefined') return;
    this.observer = new ResizeObserver(() => {
      if (this.controls === null) return;
      const next = content.getBoundingClientRect().height;
      if (next !== this.targetHeight) this.resize(host.getBoundingClientRect().height, next);
    });
    this.observer.observe(content);
    this.preference = window.matchMedia('(prefers-reduced-motion: reduce)');
    this.preference.addEventListener('change', this.onPreference);
  }

  componentWillUnmount() {
    this.observer?.disconnect();
    this.observer = null;
    this.preference?.removeEventListener('change', this.onPreference);
    this.preference = null;
    this.clear();
  }

  private onPreference = () => { if (this.preference?.matches) this.clear(); };

  private clear() {
    this.controls?.cancel();
    this.controls = null;
    this.targetHeight = null;
    const host = this.hostRef.current;
    if (host !== null) {
      host.style.height = '';
      host.style.overflow = '';
    }
  }

  private resize(from: number, to: number) {
    const host = this.hostRef.current;
    if (host === null) return;
    const velocity = this.controls?.sample().velocity ?? 0;
    this.clear();
    if ((from === to && velocity === 0) || window.matchMedia('(prefers-reduced-motion: reduce)').matches) return;
    host.style.height = `${from}px`;
    host.style.overflow = 'clip';
    this.targetHeight = to;
    const animation = playSpring([host], from, to, velocity, value => ({ height: `${Math.max(0, value)}px` }));
    this.controls = animation;
    void animation.finished.then(() => {
      if (this.controls === animation) this.clear();
    }, () => {
      // Cancellation already released the old effect; a newer one may now own the host.
    });
  }

  render() {
    return <div ref={this.hostRef} className={styles.host} data-nc-size-motion=""><div ref={this.contentRef} className={styles.content}>{this.props.children}</div></div>;
  }
}
