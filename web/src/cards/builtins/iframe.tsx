import { useEffect, useRef } from 'react';
import { z } from 'zod';
import * as api from '../../api/calm';
import { useState } from '../../shared/state';
import { dlog } from '../../util/debug';
import { CardHead } from '../CardHead';
import { useCardSlotValue, type CardEntry } from '../registry';

declare module '../../types' {
  interface WaveCardDataMap {
    iframe: IframeCardData;
  }
}

export interface IframeCardData {
  type: 'iframe';
  id: string;
  url: string;
}

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

export function iframeSandbox(rawUrl: string): string {
  const base =
    'allow-scripts allow-popups allow-forms allow-popups-to-escape-sandbox';
  try {
    const u = new URL(rawUrl, window.location.href);
    if (u.origin !== window.location.origin) {
      return `${base} allow-same-origin`;
    }
  } catch {
    /* unparseable - keep the locked-down default */
  }
  return base;
}

function IframeCard({
  card,
  onClose,
}: {
  card: IframeCardData;
  onClose?: () => void;
}) {
  const [currentUrl, setCurrentUrl] = useState(card.url);
  const [draftUrl, setDraftUrl] = useState(card.url);
  const epoch = useCardSlotValue<number>('epoch', 0);
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
        card={card}
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
      {[
        // Same-origin targets (for example, /api/plugins/...) stay opaque so they
        // can't read parent cookies. Cross-origin targets get allow-same-origin
        // so they run under their own origin, still can't reach parent cookies,
        // and can use localStorage / non-null Origin WebSockets (noVNC-style apps).
        // Keeping the iframe in a keyed child list makes the reload epoch
        // participate in reconciliation, which remounts the DOM node instead of
        // only updating attributes on the existing element.
        <iframe
          key={epoch}
          className="iframe-frame"
          src={currentUrl}
          title={`Embedded page: ${currentUrl}`}
          referrerPolicy="no-referrer"
          sandbox={iframeSandbox(currentUrl)}
        />,
      ]}
    </div>
  );
}

export const IframeEntry: CardEntry<IframeCardData> = {
  type: 'iframe',
  Component: IframeCard,
  defaultSize: { w: 6, h: 10, minW: 3, minH: 4 },
  refreshBacking: 'epoch',
  createController({ card }) {
    return {
      onVisibleChange(visible) {
        dlog('IframeCard', 'visibility', { cardId: card.id, visible });
      },
    };
  },
  claim: { mode: 'exact', kind: 'iframe' },
  title: (card) => card.url,
  accessibleName: (card) => `Web page: ${card.url}`,
  create: {
    mode: 'generic',
    buildPayload(input: { url: string }) {
      return { url: input.url };
    },
  },
  actions(_card, ctx) {
    const [, setEpoch] = ctx.useCardSlot<number>('epoch', 0);
    return [
      {
        kind: 'button',
        id: 'reload-iframe',
        label: 'Reload',
        icon: 'refresh',
        placement: 'head',
        run: () => setEpoch((epoch) => epoch + 1),
      },
    ];
  },
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
      parse(values) {
        const url = values.url.trim();
        if (!isAllowedIframeUrl(url)) {
          throw new Error(`Invalid iframe URL: ${url}`);
        }
        return { url };
      },
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
