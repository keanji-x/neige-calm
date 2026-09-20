// Images a person attaches to a planner message. Picking a file uploads it immediately; the strip
// is a list of the server's answers, and the preview is the server's own copy. Removing one before
// sending is a local forget, not a delete: the server reclaims orphans after their TTL.

import { useCallback, useEffect, useRef } from 'react';
import { Banner } from '@astryxdesign/core/Banner';
import { ChatComposerDrawer } from '@astryxdesign/core/Chat';
import { HStack } from '@astryxdesign/core/HStack';
import { IconButton } from '@astryxdesign/core/IconButton';
import { Thumbnail } from '@astryxdesign/core/Thumbnail';
import { VStack } from '@astryxdesign/core/VStack';

import { Icon } from '../../ui/icon/public.tsx';
import { useState } from '../../ui/state/public.ts';

import type {
  PlannerAttachment, UploadAttachmentResponse,
} from '../../../../core/api/generated/wire.js';
import {
  ATTACHABLE_IMAGE_TYPES, MAX_ATTACHMENTS_PER_MESSAGE,
} from '../../../../core/domain/conversation.js';
import styles from './attachments.module.css';

/** One upload, already bound to a card. A function rather than a transport: `performApiRequest` has two sanctioned call sites and a feature module is neither. */
// Capture before file reading, then consume synchronously after the final await.
// The returned reader checks the same admission before exposing server facts.
export type UploadAttachment = (readBytes: () => Promise<Uint8Array>, contentType: string)
=> Promise<() => UploadAttachmentResponse>;

/** Why a track can have no attachment surface: attachments are written into `<workspace>/.neige/`, and a workspace the person already owns is one neige never writes into (the server answers 400). */
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

/** The composer's attachment state for one card. Uploads run one at a time and in pick order. */
export function usePlannerAttachments(
  upload: UploadAttachment,
  cardId: string,
): PlannerAttachments {
  const [items, setItems] = useState<readonly PlannerAttachment[]>([]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  /* Read inside `attach`: closing over a render's `items` would let two quick picks both see an empty list and both pass the cap. */
  const live = useRef<readonly PlannerAttachment[]>([]);
  live.current = items;

  /* Bumped on every card change; an in-flight upload refuses to adopt its own answer if it no longer matches. */
  const generation = useRef(0);

  /* A picked image's bytes live under its card's directory and the server refuses it anywhere else, so moving to another conversation drops the strip. */
  useEffect(() => {
    generation.current += 1;
    setItems([]);
    setError(null);
    /* The in-flight request cannot be cancelled, but its answer is already refused by the generation check in `attach`. */
    setBusy(false);
  }, [cardId]);

  const attach = useCallback(async (file: File) => {
    const refusal = refusalFor(file.type);
    if (refusal !== null) { setError(refusal); return; }
    if (live.current.length >= MAX_ATTACHMENTS_PER_MESSAGE) {
      setError(`A message can carry at most ${MAX_ATTACHMENTS_PER_MESSAGE} images.`);
      return;
    }
    setBusy(true);
    setError(null);
    const startedAt = generation.current;
    try {
      const consume = await upload(async () => new Uint8Array(await file.arrayBuffer()), file.type);
      /* An upload that lands after the reader moved on belongs to the card it was started for. The generation counter is compared rather than the card id because the id can repeat (leave a card and come back). */
      if (generation.current !== startedAt) return;
      const uploaded = consume();
      setItems((current) => [...current, {
        id: uploaded.attachmentId, contentType: uploaded.contentType,
        size: uploaded.size, url: uploaded.url,
      }]);
    } catch (cause) {
      if (generation.current !== startedAt) return;
      setError(cause instanceof Error && cause.message !== ''
        ? cause.message : 'The image could not be uploaded.');
    } finally {
      if (generation.current === startedAt) setBusy(false);
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

/** The attach control: an `IconButton` with a hidden `<input type="file">` behind it, opened by `click()`. The unavailable reason rides on `tooltip` rather than `title`: Astryx switches a tooltipped disabled button to `aria-disabled` so it stays focusable. */
export function PlannerAttachButton({ attachments, support, disabled = false }: {
  attachments: PlannerAttachments;
  support: AttachmentSupport;
  disabled?: boolean;
}) {
  const picker = useRef<HTMLInputElement | null>(null);
  const unavailable = !support.available;
  const blocked = disabled || unavailable || attachments.busy || attachments.atCapacity;
  const tooltip = unavailable
    ? (support.reason ?? ATTACHED_WORKSPACE_REASON)
    : attachments.atCapacity
      ? `A message can carry at most ${MAX_ATTACHMENTS_PER_MESSAGE} images.`
      : undefined;
  return (
    <span className={styles.attach} data-nc-attach="">
      <IconButton
        label="Attach an image"
        icon={<Icon name="paperclip" size="sm" />}
        variant="ghost"
        size="sm"
        isDisabled={blocked}
        isLoading={attachments.busy}
        tooltip={tooltip}
        onClick={() => { picker.current?.click(); }}
      />
      <input
        ref={picker}
        className={styles.attachInput}
        type="file"
        accept={ATTACHABLE_IMAGE_TYPES.join(',')}
        /* The button above is the control; a second announced file input would be a second thing to tab to that does nothing. */
        aria-hidden="true"
        tabIndex={-1}
        onChange={(event) => {
          const file = event.target.files?.[0];
          /* Cleared unconditionally so picking the same file twice in a row still fires a change event. */
          event.target.value = '';
          if (file !== undefined) void attachments.attach(file);
        }}
      />
    </span>
  );
}

/** The strip of pending images. `Thumbnail` samples the image behind its remove button (APCA) so the button stays legible on any picture. */
export function PlannerAttachmentDrawer({ attachments }: { attachments: PlannerAttachments }) {
  const { items, busy, error } = attachments;
  if (items.length === 0 && error === null && !busy) return null;
  return (
    <ChatComposerDrawer count={items.length} label="Images">
      <VStack gap={2}>
        <HStack gap={2} wrap="wrap" data-nc-attachments="">
          {items.map((item, index) => (
            <Thumbnail
              key={item.id}
              src={item.url}
              label={`Image ${index + 1}`}
              onRemove={() => { attachments.remove(item.id); }}
            />
          ))}
          {/* An upload in flight has no url yet: the strip grows when the pick happens, not when the server answers. */}
          {busy && <Thumbnail isLoading label="Uploading" />}
        </HStack>
        {error !== null && (
          <Banner
            status="error"
            title="That image was not attached"
            description={error}
          />
        )}
      </VStack>
    </ChatComposerDrawer>
  );
}
