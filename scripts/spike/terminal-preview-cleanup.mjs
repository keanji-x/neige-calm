// Shared by the real-TUI probe and its failure-path acceptance check.
export function deadline(promise, message, ms = 8000) {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error(message)), ms);
    promise.then(value => { clearTimeout(timer); resolve(value); },
      error => { clearTimeout(timer); reject(error); });
  });
}

export async function terminateDriver(driver, exited) {
  // Signal the still-owned ChildProcess handle, never a discovered numeric PID.
  // preview-driver handles TERM by cancelling input and stopping its owned host.
  if (driver.exitCode === null && driver.signalCode === null) driver.kill('SIGTERM');
  return deadline(exited, 'driver containment cleanup timeout', 20000);
}

export async function closePreview({ browser, driver, exited, pending, stderr, drainMs = 8000 }) {
  const results = await Promise.allSettled([
    deadline(Promise.resolve().then(() => browser?.close()), 'browser shutdown timeout', 8000),
    (async () => {
      try {
        await deadline(pending, 'input drain timeout', drainMs);
        driver.stdin.end();
        const [code] = await deadline(exited, 'driver shutdown timeout', 12000);
        if (code !== 0) throw new Error(`driver exit ${code}: ${stderr()}`);
      } catch (error) {
        driver.stdin.end();
        await terminateDriver(driver, exited);
        throw error;
      }
    })(),
  ]);
  const errors = results.filter(result => result.status === 'rejected').map(result => result.reason);
  if (errors.length) throw new AggregateError(errors, 'preview cleanup failed');
}
