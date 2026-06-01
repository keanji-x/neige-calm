import { useEffect, useRef } from 'react';
import { z } from 'zod';
import * as api from '../../api/calm';
import type { IframeCardData } from '../../types';
import { useState } from '../../shared/state';
import { CardHead } from '../CardHead';
import type { CardEntry } from '../registry';

export function isAllowedIframeUrl(raw: string): boolean {
  try {
    const u = new URL(raw, window.location.href);
    return u.protocol === 'http:' || u.protocol === 'https:';
  } catch {
    return false;
  }
}

const iframePayloadSchema = z.object({
  url: z.string().min(1).refine(isAllowedIframeUrl),
});

const warnedInvalidPayloads = new Set<string>();

function IframeCard({
  card,
  onClose,
}: {
  card: IframeCardData;
  onClose?: () => void;
}) {
  const [currentUrl, setCurrentUrl] = useState(card.url);
  const [draftUrl, setDraftUrl] = useState(card.url);
  const pendingUrlRef = useRef<string | null>(null);

  useEffect(() => {
    if (pendingUrlRef.current !== null && pendingUrlRef.current === card.url) {
      pendingUrlRef.current = null;
      return;
    }
    setCurrentUrl(card.url);
    setDraftUrl(card.url);
    pendingUrlRef.current = null;
  }, [card.url]);

  const submitUrl = (e: React.FormEvent<HTMLFormElement>) => {
    e.preventDefault();
    const nextUrl = draftUrl.trim();
    if (!nextUrl) return;
    if (!isAllowedIframeUrl(nextUrl)) {
      // eslint-disable-next-line no-console
      console.warn(`[cards] iframe URL rejected for ${card.id}:`, nextUrl);
      return;
    }

    pendingUrlRef.current = nextUrl;
    setCurrentUrl(nextUrl);
    setDraftUrl(nextUrl);
    void api.updateCard(card.id, { payload: { url: nextUrl } }).catch((err: unknown) => {
      // eslint-disable-next-line no-console
      console.warn(`[cards] iframe URL persistence failed for ${card.id}:`, err);
    });
  };

  return (
    <div className="iframe-card">
      <CardHead
        className="card-drag-handle"
        title={currentUrl}
        onClose={onClose}
        closeAriaLabel="Remove panel"
      />
      <form className="iframe-url-bar" onSubmit={submitUrl}>
        <input
          className="iframe-url-input"
          type="text"
          value={draftUrl}
          placeholder="https://example.com"
          aria-label="Web page URL"
          onChange={(e) => setDraftUrl(e.target.value)}
        />
        <button className="iframe-url-go" type="submit">
          Go
        </button>
      </form>
      {/* No allow-same-origin: forces an opaque origin even on same-origin URLs,
          so an /api/plugins/... target can't read parent cookies. */}
      <iframe
        className="iframe-frame"
        src={currentUrl}
        title={`Embedded page: ${currentUrl}`}
        referrerPolicy="no-referrer"
        sandbox="allow-scripts allow-popups allow-forms allow-popups-to-escape-sandbox"
      />
    </div>
  );
}

export const IframeEntry: CardEntry<IframeCardData> = {
  type: 'iframe',
  Component: IframeCard,
  defaultSize: { w: 6, h: 10, minW: 3, minH: 4 },
  fromKernel: (k) => {
    if (k.kind !== 'iframe') return null;
    const parsed = iframePayloadSchema.safeParse(k.payload ?? {});
    if (!parsed.success) {
      if (!warnedInvalidPayloads.has(k.id)) {
        warnedInvalidPayloads.add(k.id);
        // eslint-disable-next-line no-console
        console.warn(
          `[cards] iframe payload invalid for ${k.id}:`,
          parsed.error.issues,
        );
      }
      return null;
    }
    return {
      type: 'iframe',
      id: k.id,
      url: parsed.data.url,
    };
  },
  addPanel: {
    label: 'Web page',
    createSchema: {
      fields: [
        {
          key: 'url',
          label: 'URL',
          type: 'string',
          required: true,
          placeholder: 'https://example.com',
        },
      ],
    },
  },
};
