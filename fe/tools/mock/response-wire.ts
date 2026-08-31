/** Explicit wire leaves nested below temporarily exempt legacy response envelopes. */
export type BacklinkQuote = { after: string; before: string; head_elided: boolean; label: string; tail_elided: boolean };
export type Diagnostic = { action?: string | null; code: string; message: string; messageArgs: Record<string, unknown>; path: string; relatedBlockIds: string[]; relatedWaveId?: string | null };
export type BlockVerdict = { blockId: string; childWaveDeleted?: boolean | null; childWaveId?: string | null; diagnostics: Diagnostic[]; gateResult?: unknown; key: string; schedulable: boolean; status?: string | null; statusDetail?: string | null; workerCardId?: string | null };
export type DirEntry = { is_dir: boolean; name: string };
export type GitChangedFile = { old_path?: string | null; path: string; status: string };
export type RatifyCardDecision = 'grant' | 'deny';
export type ViewSizeWire = { h: number; min_h?: number | null; min_w?: number | null; w: number };
export type WaveBacklink = { dst_block_id?: string | null; label: string; quote: BacklinkQuote; src_block_id: string; src_wave_id: string; src_wave_title: string; updated_at: number };
