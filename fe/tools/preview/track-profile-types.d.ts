/** Type-only records for the standalone diagnostic; no application globals are augmented. */
export interface BuildMetadata {
  commit: string;
  workingTreeDirty: boolean;
  reactProfiling: boolean;
  sourceMaps: boolean;
  outDir: string;
}
export interface BuildFile { path: string; bytes: number; sha256: string }
export interface BuildSource { map: string; source: string; version: number; sha256: string }
export interface BuildInput { metadata: BuildMetadata; hashes: BuildFile[]; inputs: BuildSource[]; compatVersion: number }
export interface SourceMapInput { sources?: string[]; sourcesContent?: (string | null)[] }
export interface Fixture { id: string; title: string; count: number }
export interface Click { target: string; at: number; trusted: boolean; eventAt: number; handlingDelay: number }
export interface Snapshot {
  route: string;
  target: string | null;
  shell: boolean;
  title: string | null;
  neutral: boolean;
  statuses: (string | null)[];
  report: boolean;
  firstHeading: string | null;
  headings: number;
  busy: string | null;
  full: boolean;
}
export interface Frame {
  at: number;
  frameAt: number;
  gap: number | null;
  observedGap: number | null;
  target: string | null;
  shell: boolean;
  report: boolean;
  firstHeading: string | null;
  full: boolean;
}
export interface Pulse { clickAt: number; at: number; frameAt?: number }
export interface WorkerSample {
  kind: 'create' | 'post' | 'reply' | 'error' | 'terminate';
  id: number;
  at: number;
  url?: string;
  type?: string | null;
  message?: string;
}
export interface Probe {
  recording: boolean;
  clicks: Click[];
  snapshots: (Snapshot & { at: number })[];
  frames: Frame[];
  tasks: { start: number; duration: number }[];
  workers: WorkerSample[];
  pulses: Pulse[];
  state: Snapshot | null;
}
export type BrowserScope = typeof globalThis & { __ncTrackProfile: Probe };
export interface ResourceTiming {
  url: string;
  initiatorType: string;
  start: number;
  requestStart: number;
  responseStart: number;
  responseEnd: number;
  duration: number;
  transferSize: number;
}
export type Capture = Probe & { elements: number; timeOrigin?: number; resources?: ResourceTiming[] };
export interface RecordedRequest { method: string; path: string; hostAt: number; handled: boolean }
export interface Failure { message: string; stack?: string | null }
export interface Step { step: number; target: string; attemptedAt: number; complete: boolean; superseded?: boolean }
