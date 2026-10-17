import { expect, it } from 'vitest';
import { parseGuideCatalog } from '../../ui/floating-control/public.ts';
import { workspaceGuideFacts } from './workspace-guide.tsx';
import definition from './workspace-guides.json';

it('validates the shipped catalog against the application facts it consumes', () => {
  const catalog = parseGuideCatalog(definition);
  const facts = workspaceGuideFacts(false, false);
  for (const tour of catalog.tours) for (const fact of tour.requires) expect(facts).toHaveProperty(fact);
});
