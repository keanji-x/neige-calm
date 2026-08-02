import { readFile } from 'node:fs/promises';

import { describe, expect, it } from 'vitest';

const readProjectFile = (path: string) => readFile(new URL(`../../${path}`, import.meta.url), 'utf8');

describe('review regression contracts', () => {
  it('discovers both TypeScript test extensions', async () => {
    await expect(readProjectFile('vitest.config.ts')).resolves.toContain(
      "web/src/**/*.test.{ts,tsx}",
    );
  });

  it('labels compile-only UI contracts explicitly', async () => {
    const contract = await readProjectFile('web/src/ui/state/public.contract.test.ts');
    expect(contract.match(/it\('\[type-only]/g)).toHaveLength(2);
  });

  it('documents the intentional overlay exclusions and escape hatch', async () => {
    const readme = await readProjectFile('core/state/README.md');
    expect(readme).toContain('overlay 不返回 loading');
    expect(readme).toContain('`unsafeAsPersistent`');
  });
});
