import { ErrorBox } from '../../ui/error-box/public.tsx';

/** Read recovery only: Retry repeats the selected resource read, never a write. */
export function FileReadError({ message, onRetry, resource = 'file' }: Readonly<{
  message: string;
  onRetry: () => void;
  resource?: 'file' | 'folder' | 'changes';
}>) {
  const denied = /permission denied|access denied/i.test(message);
  const missing = /not found|no such file/i.test(message);
  return <ErrorBox
    message={denied ? 'Access denied.' : missing ? 'File or folder not found.' : `Could not load this ${resource}.`}
    description={denied ? 'Check its permissions, then try again.'
      : missing ? 'Restore it at the same path, then try again.' : undefined}
    details={message}
    onRetry={onRetry}
  />;
}
