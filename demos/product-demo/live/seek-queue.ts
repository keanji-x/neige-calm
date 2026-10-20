/** One serialized native-operation lane shared by every playback/scrub/reset producer.
 * An in-flight operation finishes before the next one; obsolete pending targets are skipped.
 * Only the latest request may publish a completed playhead. */
export function createSeekQueue(
  apply: (target: number) => Promise<unknown>,
  commit: (target: number) => void,
) {
  let latest = 0;
  let tail = Promise.resolve();
  return {
    seek(target: number) {
      const id = ++latest;
      const work = tail.then(async () => {
        if (id !== latest) return false;
        await apply(target);
        if (id === latest) commit(target);
        return true;
      });
      tail = work.then(
        () => undefined,
        () => undefined,
      );
      return work;
    },
    settled() {
      return tail;
    },
  };
}
