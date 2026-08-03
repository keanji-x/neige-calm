import { existsSync, readdirSync } from 'node:fs';
import { basename, extname, resolve } from 'node:path';

export function checkCoreNoJsx(corePath = 'core', cardsPath = 'web/src/systems/cards') {
  const core = resolve(corePath);
  const jsx = existsSync(core)
    ? readdirSync(core, { recursive: true }).filter((entry) => ['.tsx', '.jsx'].includes(extname(String(entry))))
    : [];
  // Reason: INV-CARD-082 freezes registry as pure .ts because .tsx changes its documented module boundary.
  const cards = resolve(cardsPath);
  if (existsSync(cards)) {
    jsx.push(...readdirSync(cards).filter((entry) => basename(String(entry), extname(String(entry))) === 'registry'
      && ['.tsx', '.jsx'].includes(extname(String(entry)))).map((entry) => `systems/cards/${String(entry)}`));
  }
  return jsx.length ? `core-no-jsx: forbidden JSX files: ${jsx.join(', ')}` : '';
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const error = checkCoreNoJsx(process.argv[2]);
  if (error) {
    console.error(error);
    process.exitCode = 1;
  }
}
