import { expect, it } from 'vitest';
import { MAX_TASK_FILE_BYTES, taskArtifactFileOperation, taskArtifactFileSchema, taskFilePreview } from './task-artifact-file.js';

function file() { return { attemptId: 'attempt', index: 0, name: '结果.txt', size: 3, contentBase64: 'NDIK' }; }

it('scopes the one file GET to encoded Track, task, attempt and exact report index', () => {
  const operation = taskArtifactFileOperation('track/a', 'key/b', 'attempt', 0);
  expect(operation.method).toBe('GET');
  expect(operation.path).toBe('/api/tracks/track%2Fa/tasks/key%2Fb/attempts/attempt/artifacts/0');
  expect(operation.responseSchema.parse(file())).toEqual(file());
  expect(operation.responseSchema.safeParse({ ...file(), attemptId: 'other' }).success).toBe(false);
  expect(operation.responseSchema.safeParse({ ...file(), index: 1 }).success).toBe(false);
});

it('requires every DTO field and rejects extra fields', () => {
  for (const key of Object.keys(file())) {
    const missing: Record<string, unknown> = { ...file() };
    delete missing[key];
    expect(taskArtifactFileSchema.safeParse(missing).success, key).toBe(false);
  }
  expect(taskArtifactFileSchema.safeParse({ ...file(), path: '/host/private' }).success).toBe(false);
});

it.each(['', '.', '..', '../a', '/a', 'a/b', 'a\\b', 'a\0b', 'a\nb', 'a\u007fb', 'a\u0085b'])('rejects unsafe basename %j', (name) => {
  expect(taskArtifactFileSchema.safeParse({ ...file(), name }).success).toBe(false);
});

it.each(['Zg', 'Zg=', 'Zh==', 'Zm9=', 'Zm9v\n', '____', 'A===', '====', 'Zg==AAAA', 'Z g='])('rejects noncanonical base64 %j', (contentBase64) => {
  expect(taskArtifactFileSchema.safeParse({ ...file(), size: 1, contentBase64 }).success).toBe(false);
});

it('accepts empty and padded content but rejects byte-count and size-bound mismatches', () => {
  for (const [size, contentBase64] of [[0, ''], [1, 'Zg=='], [2, 'Zm8='], [3, 'Zm9v']] as const) {
    expect(taskArtifactFileSchema.parse({ ...file(), size, contentBase64 }).size).toBe(size);
  }
  expect(taskArtifactFileSchema.safeParse({ ...file(), size: 2 }).success).toBe(false);
  expect(taskArtifactFileSchema.safeParse({ ...file(), size: -1 }).success).toBe(false);
  expect(taskArtifactFileSchema.safeParse({ ...file(), size: MAX_TASK_FILE_BYTES + 1 }).success).toBe(false);
  const maximum = 'A'.repeat(11_184_811) + '=';
  expect(taskArtifactFileSchema.safeParse({ ...file(), size: MAX_TASK_FILE_BYTES, contentBase64: maximum }).success).toBe(true);
  expect(taskArtifactFileSchema.safeParse({ ...file(), size: MAX_TASK_FILE_BYTES, contentBase64: maximum + 'AAAA' }).success).toBe(false);
});

it('bounds preview by Unicode characters, preserves empty text, and excludes NUL anywhere', () => {
  expect(taskFilePreview('')).toEqual({ text: '', truncated: false });
  expect(taskFilePreview('<script>no execution</script>')).toEqual({ text: '<script>no execution</script>', truncated: false });
  expect(taskFilePreview('😀'.repeat(65_537))).toEqual({ text: '😀'.repeat(65_536), truncated: true });
  expect(taskFilePreview('x'.repeat(65_536))).toEqual({ text: 'x'.repeat(65_536), truncated: false });
  expect(taskFilePreview('x'.repeat(65_537) + '\0')).toBeNull();
});
