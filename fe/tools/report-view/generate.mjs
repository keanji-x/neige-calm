import { readFileSync, writeFileSync, mkdtempSync, symlinkSync, rmSync } from 'node:fs';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import ts from 'typescript';
import { z } from 'zod';

const root = fileURLToPath(new URL('../../', import.meta.url));
const output = fileURLToPath(new URL('../../core/domain/report-view.schema.json', import.meta.url));
const temporary = mkdtempSync(join(tmpdir(), 'neige-native-schema-'));
try {
  writeFileSync(join(temporary, 'package.json'), '{"type":"module"}');
  symlinkSync(join(root, 'node_modules'), join(temporary, 'node_modules'), 'dir');
  for (const file of ['report-date', 'report-table', 'report-view']) {
    const source = readFileSync(join(root, 'core/domain', `${file}.ts`), 'utf8');
    const result = ts.transpileModule(source, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext } });
    writeFileSync(join(temporary, `${file}.js`), result.outputText);
  }
  const { nativeViewPayloadSchema } = await import(pathToFileURL(join(temporary, 'report-view.js')).href);
  const schema = z.toJSONSchema(nativeViewPayloadSchema);
  schema.description = 'Native read-only composition. Additionally validated: unique ids, real increasing UTC dates, point width equals series count, complete nonnegative stacked values, layout/cell count, at most one primary metric, and 256 KiB total canonical JSON.';
  const encoded = JSON.stringify(schema, null, 2) + '\n';
  if (process.argv.includes('--check')) {
    if (readFileSync(output, 'utf8') !== encoded) throw new Error('Native view schema drift; run node tools/report-view/generate.mjs');
  } else writeFileSync(output, encoded);
} finally { rmSync(temporary, { recursive: true, force: true }); }
