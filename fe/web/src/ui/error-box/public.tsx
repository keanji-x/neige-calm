export function ErrorBox({ message, onRetry }: { message: string; onRetry: () => void }) {
  return (
    <div role="alert" data-nc-error-box="">
      <span aria-hidden="true">●</span>
      <span>{message}</span>
      <button type="button" data-nc-action="tertiary" onClick={onRetry}>Retry</button>
    </div>
  );
}
