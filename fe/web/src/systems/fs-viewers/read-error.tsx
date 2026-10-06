import type { ApiFailure } from '../../../../core/api/types.ts';
import { classifyFailure } from '../../../../core/domain/failure-class.ts';
import { FILE_READ_FAILURES, type FileReadClass } from '../../../../core/domain/fs.ts';
import { readFailureText } from '../../../../core/domain/read-failure.ts';
import { ErrorBox } from '../../ui/error-box/public.tsx';

/** What the read was of: a text file, an image the browser loaded itself, a folder listing, or a file's changes. */
export type FileReadResource = 'file' | 'image' | 'folder' | 'changes';

const UNREADABLE: Readonly<Record<FileReadResource, string>> = Object.freeze({
  file: 'Could not load this file.',
  image: 'Could not read this image.',
  folder: 'Could not load this folder.',
  changes: 'Could not load these changes.',
});

/** Each class's sentence and what the reader can do about it; `other` says what could not be read. */
const BY_CLASS: Readonly<Record<Exclude<FileReadClass, 'other'>, Readonly<{ sentence: string; hint: string }>>> = Object.freeze({
  denied: Object.freeze({ sentence: 'Access denied.', hint: 'Check its permissions, then try again.' }),
  missing: Object.freeze({ sentence: 'File or folder not found.', hint: 'Restore it at the same path, then try again.' }),
  gone: Object.freeze({ sentence: 'This Track no longer exists.', hint: 'Its workspace files cannot be opened from here.' }),
});

/**
 * Read recovery only: Retry repeats the selected resource read, never a write. Which sentence shows is the failure's
 * class on the fs routes (`FILE_READ_FAILURES`), never its wording; the read rule adds the server's reason to it.
 */
export function FileReadError({ failure, onRetry, resource = 'file' }: Readonly<{
  failure: ApiFailure | null;
  onRetry: () => void;
  resource?: FileReadResource;
}>) {
  const is = classifyFailure(failure, FILE_READ_FAILURES);
  const known = is === 'other' ? null : BY_CLASS[is];
  return <ErrorBox
    message={readFailureText(failure, known?.sentence ?? UNREADABLE[resource])}
    description={known?.hint}
    onRetry={onRetry}
  />;
}
