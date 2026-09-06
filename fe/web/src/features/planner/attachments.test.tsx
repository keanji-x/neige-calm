// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
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

function Harness({ upload, supported = true }: { upload: UploadAttachment; supported?: boolean }) {
  const attachments = usePlannerAttachments(upload, 'card-1');
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

function picker(): HTMLInputElement {
  return screen.getByLabelText<HTMLInputElement>('Attach an image');
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
    const thumb = screen.getByRole('presentation', { hidden: true })
      ?? screen.getAllByRole('img', { hidden: true })[0];
    expect(thumb.getAttribute('src')).toBe(uploaded(0).url);
    expect(latest?.ids).toEqual([uploaded(0).attachmentId]);
  });

  it('removes a picked image before it is ever sent', async () => {
    const upload = vi.fn<UploadAttachment>().mockResolvedValue(uploaded(0));
    render(<Harness upload={upload} />);
    await pick(png());
    expect(latest?.ids).toHaveLength(1);

    act(() => { fireEvent.click(screen.getByLabelText('Remove this image')); });
    expect(latest?.ids).toEqual([]);
    expect(screen.queryByLabelText('Remove this image')).toBeNull();
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
    expect(picker().disabled).toBe(true);

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
   * The acceptance condition #1505 set for a track whose workspace is a folder
   * the person owns: the control says it is unavailable AND says why, rather
   * than looking live and answering 400.
   */
  it('is unavailable with a reason on a track that cannot take attachments', () => {
    render(<Harness upload={vi.fn<UploadAttachment>()} supported={false} />);
    expect(picker().disabled).toBe(true);
    expect(screen.getByTitle(ATTACHED_WORKSPACE_REASON)).toBeTruthy();
  });
});
