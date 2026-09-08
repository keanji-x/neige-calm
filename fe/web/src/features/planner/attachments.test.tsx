// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { UploadAttachmentResponse } from '../../../../core/api/generated/wire.ts';
import {
  ATTACHED_WORKSPACE_REASON, PlannerAttachButton, PlannerAttachmentDrawer, usePlannerAttachments,
  type PlannerAttachments, type UploadAttachment,
} from './attachments.tsx';

afterEach(cleanup);

function uploaded(index: number): UploadAttachmentResponse {
  const id = `0189bc3f-2b1a-4c7d-9e4f-1a2b3c4d5e6${index}.png`;
  return {
    attachmentId: id, contentType: 'image/png', size: 12,
    url: `/api/cards/card-1/planner/attachments/${id}`,
  };
}

/** A png `File` whose bytes never matter — the server decides the format. */
function png(name = 'shot.png'): File {
  return new File([new Uint8Array([0x89, 0x50, 0x4e, 0x47])], name, { type: 'image/png' });
}

let latest: PlannerAttachments | null = null;

function Harness({ upload, supported = true, card = 'card-1' }: {
  upload: UploadAttachment; supported?: boolean; card?: string;
}) {
  const attachments = usePlannerAttachments(upload, card);
  latest = attachments;
  return (
    <>
      <PlannerAttachButton
        attachments={attachments}
        support={{ available: supported, reason: ATTACHED_WORKSPACE_REASON }}
      />
      <PlannerAttachmentDrawer attachments={attachments} />
    </>
  );
}

/**
 * The hidden `<input type="file">`.
 *
 * Found by its type and not by an accessible name, because it deliberately has
 * none: the `IconButton` beside it is the control a person operates, carries
 * the name and the disabled state, and opens this by `click()`. Two announced
 * file controls in one composer was the alternative.
 */
function picker(): HTMLInputElement {
  const found = document.querySelector<HTMLInputElement>('input[type="file"]');
  if (found === null) throw new Error('no file input rendered');
  return found;
}

/** The control itself. */
function attachButton(): HTMLButtonElement {
  return screen.getByRole<HTMLButtonElement>('button', { name: 'Attach an image' });
}

/** True whether the button is natively disabled or `aria-disabled` (which is
 *  what Astryx switches to when the button carries a tooltip, so that the
 *  reason stays reachable by keyboard). */
function attachBlocked(): boolean {
  const button = attachButton();
  return button.disabled || button.getAttribute('aria-disabled') === 'true';
}

async function pick(file: File) {
  await act(async () => {
    fireEvent.change(picker(), { target: { files: [file] } });
    /* The upload is a promise; letting the microtask queue drain inside `act`
       is what folds its state update into this commit. */
    await Promise.resolve();
  });
}

describe('planner attachments', () => {
  /*
   * The control opens the picker.
   *
   * Every other test here dispatches `change` on the hidden input directly,
   * which is the only way to simulate a file choice — and which means none of
   * them execute the line that opens it. Delete `picker.current?.click()` and
   * they all still pass while nobody can attach anything.
   */
  it('opens the file picker when the control is pressed', async () => {
    render(<Harness upload={vi.fn<UploadAttachment>()} />);
    const opened = vi.fn();
    picker().addEventListener('click', opened);
    await userEvent.click(attachButton());
    expect(opened).toHaveBeenCalledTimes(1);
  });

  it('uploads on pick and previews the server copy, not a local one', async () => {
    const upload = vi.fn<UploadAttachment>().mockResolvedValue(uploaded(0));
    render(<Harness upload={upload} />);
    await pick(png());

    expect(upload).toHaveBeenCalledTimes(1);
    const [bytes, contentType] = upload.mock.calls[0] ?? [];
    expect(bytes).toBeInstanceOf(Uint8Array);
    expect(contentType).toBe('image/png');

    /*
     * The preview src is the url the SERVER built. Anything derived locally —
     * a blob url, a data url — would be a second representation of the same
     * bytes, and on this deployment (plain-http LAN) the secure-context-only
     * half of that family does not exist at runtime at all.
     */
    const thumb = document.querySelector('img');
    expect(thumb?.getAttribute('src')).toBe(uploaded(0).url);
    expect(latest?.ids).toEqual([uploaded(0).attachmentId]);
  });

  it('removes a picked image before it is ever sent', async () => {
    const upload = vi.fn<UploadAttachment>().mockResolvedValue(uploaded(0));
    render(<Harness upload={upload} />);
    await pick(png());
    expect(latest?.ids).toHaveLength(1);

    act(() => { fireEvent.click(screen.getByLabelText('Remove Image 1')); });
    expect(latest?.ids).toEqual([]);
    expect(screen.queryByLabelText('Remove Image 1')).toBeNull();
  });

  it('refuses a file that is not one of the four formats without a round trip', async () => {
    const upload = vi.fn<UploadAttachment>().mockResolvedValue(uploaded(0));
    render(<Harness upload={upload} />);
    await pick(new File(['note'], 'notes.txt', { type: 'text/plain' }));

    expect(upload).not.toHaveBeenCalled();
    expect(screen.getByRole('alert').textContent).toContain('PNG, JPEG, GIF or WebP');
  });

  it('stops at eight and says so', async () => {
    const upload = vi.fn<UploadAttachment>()
      .mockImplementation(() => Promise.resolve(uploaded(latest?.ids.length ?? 0)));
    render(<Harness upload={upload} />);
    for (let index = 0; index < 8; index += 1) await pick(png(`shot-${index}.png`));
    expect(latest?.ids).toHaveLength(8);
    expect(latest?.atCapacity).toBe(true);
    expect(attachBlocked()).toBe(true);

    upload.mockClear();
    await pick(png('ninth.png'));
    expect(upload).not.toHaveBeenCalled();
  });

  it('shows the server refusal rather than a generic failure', async () => {
    const upload = vi.fn<UploadAttachment>()
      .mockRejectedValue(new Error('attachment budget exhausted for this card (64 MiB).'));
    render(<Harness upload={upload} />);
    await pick(png());
    expect(screen.getByRole('alert').textContent).toContain('budget exhausted');
    expect(latest?.ids).toEqual([]);
  });

  /*
   * #1505 S6 review — an upload that lands after the reader moved on belongs
   * to the card it was started for.
   *
   * The card-change effect clears the strip, but it cannot cancel a request
   * already in flight. Without an ownership check the answer appended to the
   * NEW card's composer: card B showing card A's picture, and a cross-card
   * refusal when it is sent.
   */
  it('does not adopt an upload that finished after the card changed', async () => {
    let settle: ((value: UploadAttachmentResponse) => void) | undefined;
    const upload = vi.fn<UploadAttachment>()
      .mockImplementation(() => new Promise((resolve) => { settle = resolve; }));
    const { rerender } = render(<Harness upload={upload} card="card-1" />);
    await pick(png());
    expect(upload).toHaveBeenCalledTimes(1);
    expect(latest?.ids).toEqual([]);

    rerender(<Harness upload={upload} card="card-2" />);
    await act(async () => {
      settle?.(uploaded(0));
      await Promise.resolve();
    });
    expect(latest?.ids).toEqual([]);
    expect(screen.queryByLabelText('Remove Image 1')).toBeNull();
  });

  it('does adopt an upload that finished while the same card was still open', async () => {
    let settle: ((value: UploadAttachmentResponse) => void) | undefined;
    const upload = vi.fn<UploadAttachment>()
      .mockImplementation(() => new Promise((resolve) => { settle = resolve; }));
    render(<Harness upload={upload} card="card-1" />);
    await pick(png());
    await act(async () => {
      settle?.(uploaded(0));
      await Promise.resolve();
    });
    expect(latest?.ids).toEqual([uploaded(0).attachmentId]);
  });

  /*
   * The acceptance condition #1505 set for a track whose workspace is a folder
   * the person owns: the control says it is unavailable AND says why, rather
   * than looking live and answering 400.
   */
  it('is unavailable with a reason on a track that cannot take attachments', async () => {
    render(<Harness upload={vi.fn<UploadAttachment>()} supported={false} />);
    expect(attachBlocked()).toBe(true);
    /*
     * The reason is a tooltip on the button rather than a `title` attribute,
     * and the button stays focusable while disabled so the tooltip can be
     * reached without a mouse — which is the half `title` never had. Asserted
     * by actually opening it: a tooltip nobody can open is not a reason.
     */
    await userEvent.hover(attachButton());
    expect(await screen.findByText(ATTACHED_WORKSPACE_REASON)).toBeTruthy();
  });
});
