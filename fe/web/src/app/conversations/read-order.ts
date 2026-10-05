import { replaceEqualDeep } from '@tanstack/react-query';

/**
 * One query's reads, numbered in the tab's one read order as each starts (#1923 S2, #2043). A query function returns
 * a `Read` (the run, or one transcript page), and `keyOf` finds the read a stored result stands for (the run itself,
 * or the transcript's newest page). A result carries the number of the read it came from; one this view did not read
 * carries 0, which nothing retires by.
 */
export function numberReads<Read extends object, Result>(nextRead: () => number, keyOf: (result: Result) => Read | undefined) {
  let started = 0;
  /* Structural sharing can store a value other than the one read (the stored one when nothing changed, a copy
     otherwise), so each stored result stamps what it keeps with the number of the read it came from. */
  const starts = new WeakMap<object, number>();
  const startOf = (result: Result | undefined) => {
    const read = result === undefined ? undefined : keyOf(result);
    return read === undefined ? 0 : starts.get(read) ?? 0;
  };
  return {
    queryFn: <Context>(queryFn: (context: Context) => Promise<Read>) => async (context: Context): Promise<Read> => {
      const start = nextRead();
      started = start;
      const read = await queryFn(context);
      starts.set(read, start);
      return read;
    },
    structuralSharing: (previous: unknown, next: unknown): unknown => {
      const shared = replaceEqualDeep(previous, next) as Result;
      const read = keyOf(shared);
      if (read !== undefined) starts.set(read, startOf(next as Result));
      return shared;
    },
    startOf,
    /** The number of the latest read started so far. */
    latest: () => started,
  };
}
