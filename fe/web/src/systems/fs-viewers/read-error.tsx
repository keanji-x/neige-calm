import type { ApiFailure } from '../../../../core/api/types.ts';
import { classifyFailure } from '../../../../core/domain/failure-class.ts';
import { FILE_READ_FAILURES } from '../../../../core/domain/fs.ts';
import { readFailureText } from '../../../../core/domain/read-failure.ts';
import { ErrorBox } from '../../ui/error-box/public.tsx';

/**
 * Read recovery only: Retry repeats the selected resource read, never a write. Which sentence shows is the failure's
 * class on the fs routes (`FILE_READ_FAILURES`), never its wording; the read rule adds the server's reason to it.
 */
export function FileReadError({ failure, onRetry, resource = 'file' }: Readonly<{
  failure: ApiFailure | null;
  onRetry: () => void;
  resource?: 'file' | 'folder' | 'changes';
}>) {
  const is = classifyFailure(failure, FILE_READ_FAILURES);
  const sentence = is === 'denied' ? 'Access denied.' : is === 'missing' ? 'File or folder not found.'
    : `Could not load ${resource === 'changes' ? 'these changes' : `this ${resource}`}.`;
  return <ErrorBox
    message={readFailureText(failure, sentence)}
    description={is === 'denied' ? 'Check its permissions, then try again.'
      : is === 'missing' ? 'Restore it at the same path, then try again.' : undefined}
    onRetry={onRetry}
  />;
}
