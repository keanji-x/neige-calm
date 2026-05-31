import { useEffect } from 'react';
import { z } from 'zod';
import * as api from '../../api/calm';
import type { IframeCardData } from '../../types';
import { useState } from '../../shared/state';
import { CardHead } from '../CardHead';
import type { CardEntry } from '../registry';

const iframePayloadSchema = z.object({
  url: z.string().min(1),
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

  useEffect(() => {
    setCurrentUrl(card.url);
    setDraftUrl(card.url);
  }, [card.id, card.url]);

  const submitUrl = (e: React.FormEvent<HTMLFormElement>) => {
    e.preventDefault();
    const nextUrl = draftUrl.trim();
    if (!nextUrl) return;

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
      <iframe
        className="iframe-frame"
        src={currentUrl}
        title={`Embedded page: ${currentUrl}`}
        referrerPolicy="no-referrer"
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
