// #1505 S6 — images a person attaches to a planner message.
//
// ── What an attachment is, in this UI ──────────────────────────────────────
//
// It is a thing that already exists on the server before it is part of a
// message. Picking a file uploads it immediately and the server answers with
// an id and a url; the strip below the composer is a list of those answers,
// not a list of local files waiting for a send. Two things follow, and both
// are the reason it is built this way:
//
// **The preview is the server's own copy.** `<img src>` points at the
// read-back endpoint, so what the reader is looking at before they send is the
// exact bytes codex will be handed. Nothing here calls `URL.createObjectURL`,
// reads a `FileReader` data url, or holds a second representation that could
// disagree with the first — and nothing here calls `crypto.randomUUID` either.
// Ids are minted by the server. (Production is plain-http LAN, where the
// secure-context-only web APIs are simply absent; jsdom is a secure context,
// so a test passing proves nothing about that. The design here does not
// depend on the distinction, which is the point.)
//
// **Removing one before sending is a local forget, not a delete.** There is no
// server-side delete: an uploaded attachment that is never named by a message
// stays in the server's staging area and is reclaimed after its orphan TTL. So
// `remove` drops it from this list and lets it expire. It costs the card's
// attachment budget until then (#1505 GAP-A12), which is a cost worth naming
// rather than a bug to fix here.

import { useCallback, useEffect, useRef } from 'react';
import { useState } from '../../ui/state/public.ts';
import { ChatComposerDrawer } from '@astryxdesign/core/Chat';

import type {
  PlannerAttachment, UploadAttachmentResponse,
} from '../../../../core/api/generated/wire.js';
import {
  ATTACHABLE_IMAGE_TYPES, MAX_ATTACHMENTS_PER_MESSAGE,
} from '../../../../core/domain/conversation.js';
import styles from './attachments.module.css';

/**
 * One upload, already bound to a card.
 *
 * A function rather than a transport, because `performApiRequest` has exactly
 * two sanctioned call sites in this app (`app/providers/queries.ts` and the
 * session probe) and a feature module is neither. The store hands this down;
 * what arrives here is the answer or a throw.
 */
export type UploadAttachment = (bytes: Uint8Array, contentType: string)
=> Promise<UploadAttachmentResponse>;

/**
 * Why a track can have no attachment surface at all.
 *
 * Attachments are written into `<workspace>/.neige/`, and a track whose
 * workspace is a directory the person already owns is one neige never writes
 * into. The server answers 400 there. Saying so up front — a disabled control
 * with a reason — is the acceptance condition rather than a nicety: half the
 * tracks in production were created with a `cwd` and are attached, so a
 * silently-failing paperclip would be the common case, not the edge.
 */
export type AttachmentSupport = Readonly<{ available: boolean; reason?: string }>;

export const ATTACHED_WORKSPACE_REASON =
  'This track works in a folder you own, and neige never writes into one. Images can be attached on tracks with a managed workspace.';

export type PlannerAttachments = Readonly<{
  items: readonly PlannerAttachment[];
  /** Ids in the order they will be sent. */
  ids: readonly string[];
  attach: (file: File) => Promise<void>;
  remove: (id: string) => void;
  clear: () => void;
  busy: boolean;
  /** Last refusal, shown beside the strip. Cleared by the next successful pick. */
  error: string | null;
  atCapacity: boolean;
}>;

function refusalFor(contentType: string): string | null {
  if ((ATTACHABLE_IMAGE_TYPES as readonly string[]).includes(contentType)) return null;
  return 'That file is not one of PNG, JPEG, GIF or WebP.';
}

/**
 * The composer's attachment state for one card.
 *
 * Uploads run one at a time and in pick order. The server serializes a card's
 * uploads behind its own lock anyway, so firing several at once would only
 * make the strip fill in an order the reader did not choose.
 */
export function usePlannerAttachments(
  upload: UploadAttachment,
  cardId: string,
): PlannerAttachments {
  const [items, setItems] = useState<readonly PlannerAttachment[]>([]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  /* Read inside `attach`, which closes over a render's `items` otherwise and
     would let two quick picks both see an empty list and both pass the cap. */
  const live = useRef<readonly PlannerAttachment[]>([]);
  live.current = items;

  /* A picked image belongs to the card it was uploaded to — its bytes live
     under that card's directory and the server refuses it anywhere else — so
     moving to another conversation drops the strip rather than carrying it. */
  useEffect(() => { setItems([]); setError(null); }, [cardId]);

  const attach = useCallback(async (file: File) => {
    const refusal = refusalFor(file.type);
    if (refusal !== null) { setError(refusal); return; }
    if (live.current.length >= MAX_ATTACHMENTS_PER_MESSAGE) {
      setError(`A message can carry at most ${MAX_ATTACHMENTS_PER_MESSAGE} images.`);
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const bytes = new Uint8Array(await file.arrayBuffer());
      const uploaded = await upload(bytes, file.type);
      setItems((current) => [...current, {
        id: uploaded.attachmentId, contentType: uploaded.contentType,
        size: uploaded.size, url: uploaded.url,
      }]);
    } catch (cause) {
      /* The server's own sentence when it has one — it says which of the four
         refusals this was (wrong format, attached workspace, budget, size),
         and every one of those is something the reader can act on. */
      setError(cause instanceof Error && cause.message !== ''
        ? cause.message : 'The image could not be uploaded.');
    } finally {
      setBusy(false);
    }
  }, [upload]);

  const remove = useCallback((id: string) => {
    setItems((current) => current.filter((item) => item.id !== id));
  }, []);
  const clear = useCallback(() => { setItems([]); setError(null); }, []);

  return {
    items,
    ids: items.map((item) => item.id),
    attach,
    remove,
    clear,
    busy,
    error,
    atCapacity: items.length >= MAX_ATTACHMENTS_PER_MESSAGE,
  };
}

/**
 * The paperclip.
 *
 * A label wrapping a hidden `<input type="file">` rather than a button that
 * calls `click()` on one: the native control is the thing that opens the
 * picker, and driving it programmatically is the version that stops working
 * the moment a browser decides the call was not user-initiated.
 */
export function PlannerAttachButton({ attachments, support, disabled = false }: {
  attachments: PlannerAttachments;
  support: AttachmentSupport;
  disabled?: boolean;
}) {
  const unavailable = !support.available;
  const blocked = disabled || unavailable || attachments.busy || attachments.atCapacity;
  const title = unavailable
    ? (support.reason ?? ATTACHED_WORKSPACE_REASON)
    : attachments.atCapacity
      ? `A message can carry at most ${MAX_ATTACHMENTS_PER_MESSAGE} images.`
      : 'Attach an image';
  return (
    <label className={styles.attach} title={title} data-nc-attach="">
      <span className={styles.attachGlyph} aria-hidden="true">＋</span>
      <span className={styles.attachLabel}>Image</span>
      <input
        className={styles.attachInput}
        type="file"
        accept={ATTACHABLE_IMAGE_TYPES.join(',')}
        aria-label="Attach an image"
        disabled={blocked}
        onChange={(event) => {
          const file = event.target.files?.[0];
          /* Cleared unconditionally so picking the same file twice in a row
             still fires a change event. */
          event.target.value = '';
          if (file !== undefined) void attachments.attach(file);
        }}
      />
    </label>
  );
}

/** The strip of pending images, in the composer's drawer slot. */
export function PlannerAttachmentDrawer({ attachments }: { attachments: PlannerAttachments }) {
  if (attachments.items.length === 0 && attachments.error === null) return null;
  return (
    <ChatComposerDrawer count={attachments.items.length} label="Images">
      <ul className={styles.strip} data-nc-attachments="">
        {attachments.items.map((item) => (
          <li key={item.id} className={styles.item}>
            <img className={styles.thumb} src={item.url} alt="" />
            <button
              type="button"
              className={styles.remove}
              aria-label="Remove this image"
              onClick={() => { attachments.remove(item.id); }}
            >×</button>
          </li>
        ))}
      </ul>
      {attachments.error !== null && (
        <p className={styles.error} role="alert">{attachments.error}</p>
      )}
    </ChatComposerDrawer>
  );
}
