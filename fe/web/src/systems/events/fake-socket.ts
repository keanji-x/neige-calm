export type FakeSocketFactoryOptions = Readonly<{ throwOnConstruct?: () => Error | undefined }>;

export class FakeSocket {
  onopen: ((event: Event) => void) | null = null;
  onmessage: ((event: MessageEvent) => void) | null = null;
  onclose: ((event: CloseEvent) => void) | null = null;
  onerror: ((event: Event) => void) | null = null;
  readonly sent: string[] = [];
  closeCount = 0;

  send(value: string): void { this.sent.push(value); }
  open(): void { this.onopen?.(new Event('open')); }
  message(data: unknown): void { this.onmessage?.(new MessageEvent('message', { data })); }
  error(): void { this.onerror?.(new Event('error')); }
  close(): void {
    this.closeCount += 1;
    const handler = this.onclose;
    setTimeout(() => handler?.(new CloseEvent('close')), 0);
  }
}

export function createFakeSocketFactory(options: FakeSocketFactoryOptions = {}) {
  const sockets: FakeSocket[] = [];
  let constructionCount = 0;
  const factory = (url: string): FakeSocket => {
    void url;
    constructionCount += 1;
    const error = options.throwOnConstruct?.();
    if (error !== undefined) throw error;
    const socket = new FakeSocket();
    sockets.push(socket);
    return socket;
  };
  return {
    factory,
    sockets,
    get constructionCount() { return constructionCount; },
    get closeCount() { return sockets.reduce((total, socket) => total + socket.closeCount, 0); },
  };
}
