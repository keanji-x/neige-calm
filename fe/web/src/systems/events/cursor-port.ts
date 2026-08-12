export interface SyncCursorPort {
  read(): number | null;
  write(cursor: number | null): void;
  adopt(dbInstanceId: string): void;
  clear(): void;
}
