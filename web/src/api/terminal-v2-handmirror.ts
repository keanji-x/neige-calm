// HAND-MIRROR of v2 wire types from crates/calm-session/src/lib.rs.
// Source of truth is Rust; this file is consumed by web in PR-3 when XtermView
// switches to v2. ts-rs replacement is also scheduled for PR-3.
// IF YOU EDIT THIS, also edit the Rust types and verify with cargo test.
//
// JSON shape matches serde's default for the corresponding Rust enums and
// structs: tagged unions use externally-tagged ("VariantName": value) form,
// `Vec<u8>` becomes `number[]` (JSON array of bytes), `Uuid` is a string,
// optional fields become `field?: T | null`.

export const PROTOCOL_VERSION = 2 as const;

export type Role = "Owner" | "Observer";

export type RenderEncoding = "Vt";

export type InitialScrollback =
  | "None"
  | "All"
  | { Lines: number };

export interface PtySize {
  cols: number;
  rows: number;
  pixel_width: number | null;
  pixel_height: number | null;
}

export interface CellSize {
  width: number;
  height: number;
}

export interface ResumeFrom {
  render_rev: number | null;
  pty_seq: number | null;
}

export interface ClientCapabilities {
  render_encodings: RenderEncoding[];
  supports_scrollback: boolean;
  supports_sixel: boolean;
  supports_images: boolean;
}

export interface RenderSnapshot {
  render_rev: number;
  pty_seq: number;
  cols: number;
  rows: number;
  encoding: RenderEncoding;
  /** Raw PTY bytes (encoding=Vt). PR-2 swaps for cell-grid diffs. */
  data: number[];
  scrollback: number[] | null;
}

export interface RenderPatch {
  render_rev: number;
  prev_render_rev: number;
  pty_seq: number;
  encoding: RenderEncoding;
  data: number[];
}

export interface HistoryGap {
  requested_render_rev: number | null;
  requested_pty_seq: number | null;
  earliest_render_rev: number;
  earliest_pty_seq: number;
  requires_snapshot: boolean;
}

export type BackpressurePolicy =
  | "LatestOnly"
  | "SnapshotRequired"
  | "Close";

export type ProtocolErrorCode =
  | "UnsupportedVersion"
  | "NotOwner"
  | "BadSequence"
  | "SnapshotMissing"
  | "UnsupportedEncoding"
  | "BadHandshake";

// ---- ClientMsg (externally tagged) -------------------------------------

export type ClientMsg =
  | {
      ClientHello: {
        protocol_version: number;
        terminal_id: string;
        client_id: string;
        desired_size: PtySize;
        cell_size: CellSize | null;
        initial_scrollback: InitialScrollback;
        resume_from: ResumeFrom | null;
        role_hint: Role | null;
        capabilities: ClientCapabilities;
      };
    }
  | { Input: number[] }
  | { ResizeCommit: { epoch: number; cols: number; rows: number } }
  | "OwnerClaim"
  | "OwnerRelease"
  | { RenderAck: { render_rev: number; pty_seq: number | null } }
  | "Kill"
  | { ChatUserMessage: { content: string } }
  | "ChatStop"
  | {
      AnswerQuestion: {
        question_id: string;
        answers: Record<string, string>;
      };
    };

// ---- DaemonMsg (externally tagged) -------------------------------------

export type DaemonMsg =
  | {
      ServerHello: {
        protocol_version: number;
        terminal_id: string;
        session_id: string;
        client_role: Role;
        owner_client_id: string | null;
        pty_size: PtySize;
        pty_seq_head: number;
        pty_seq_tail: number;
        render_rev: number;
        snapshot: RenderSnapshot;
        history_gap: HistoryGap | null;
      };
    }
  | { RenderSnapshot: RenderSnapshot }
  | { RenderPatch: RenderPatch }
  | {
      ResizeApplied: {
        epoch: number;
        pty_seq: number;
        render_rev: number;
        cols: number;
        rows: number;
      };
    }
  | { OwnerChanged: { owner_client_id: string | null } }
  | { Backpressure: { policy: BackpressurePolicy } }
  | { SnapshotRequired: { reason: string } }
  | {
      TerminalExited: {
        code: number | null;
        pty_seq: number;
        render_rev: number;
      };
    }
  | {
      ProtocolError: {
        code: ProtocolErrorCode;
        message: string;
        expected_version: number | null;
      };
    }
  | { HelloChat: { replay: string[] } }
  | { ChatEvent: { json: string } }
  | { ChildExited: { code: number | null } };
