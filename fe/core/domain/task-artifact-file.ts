import { z } from 'zod';
import type { ApiOperation } from '../api/types.js';

export const MAX_TASK_FILE_BYTES = 8_388_608;
export const MAX_TASK_FILE_BASE64 = 11_184_812;
export const MAX_TASK_FILE_PREVIEW = 65_536;

/** Canonical standard padded base64, including zero padding bits; no platform decoder needed. */
function decodedSize(value: string): number | null {
  if (value.length > MAX_TASK_FILE_BASE64 || value.length % 4 !== 0) return null;
  if (value === '') return 0;
  const padding = value.endsWith('==') ? 2 : value.endsWith('=') ? 1 : 0;
  const body = value.slice(0, value.length - padding);
  if (/[^A-Za-z0-9+/]/.test(body)) return null;
  const last = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/'.indexOf(body.at(-1) ?? '');
  if ((padding === 2 && last % 16 !== 0) || (padding === 1 && last % 4 !== 0)) return null;
  return value.length / 4 * 3 - padding;
}

export const taskArtifactFileSchema = z.strictObject({
  attemptId: z.string().min(1),
  index: z.number().int().nonnegative(),
  name: z.string().min(1).refine((name) => name !== '.' && name !== '..' && !/[\p{Cc}/\\]/u.test(name),
    { message: 'File name must be a safe basename' }),
  size: z.number().int().min(0).max(MAX_TASK_FILE_BYTES),
  contentBase64: z.string().max(MAX_TASK_FILE_BASE64),
}).refine((file) => decodedSize(file.contentBase64) === file.size,
  { message: 'File content is not canonical base64 of the declared byte count' });
export type TaskArtifactFile = z.infer<typeof taskArtifactFileSchema>;
export type TaskFilePreview = Readonly<{ text: string; truncated: boolean }>;

/** The only address is an exact report index. Model-provided references never enter this path. */
export function taskArtifactFileOperation(trackId: string, taskKey: string, attemptId: string, index: number): ApiOperation<TaskArtifactFile> {
  return { method: 'GET',
    path: `/api/tracks/${encodeURIComponent(trackId)}/tasks/${encodeURIComponent(taskKey)}/attempts/${encodeURIComponent(attemptId)}/artifacts/${index}`,
    responseSchema: taskArtifactFileSchema.refine((file) => file.attemptId === attemptId && file.index === index,
      { message: 'File response belongs to a different attempt or artifact index' }),
  };
}

/** Input has already passed fatal UTF-8 decoding. Count code points without splitting a surrogate pair. */
export function taskFilePreview(text: string): TaskFilePreview | null {
  if (text.includes('\0')) return null;
  let end = 0;
  for (let count = 0; count < MAX_TASK_FILE_PREVIEW && end < text.length; count += 1) {
    end += text.codePointAt(end)! > 0xffff ? 2 : 1;
  }
  return { text: text.slice(0, end), truncated: end < text.length };
}
