import { expect, it } from 'vitest';
import { parseGuideCatalog } from '../../ui/floating-control/public.ts';
import { workspaceGuideFacts } from './workspace-guide.tsx';
import definition from './workspace-guides.json';

it('validates the shipped catalog against the application facts it consumes', () => {
  const catalog = parseGuideCatalog(definition);
  const facts = workspaceGuideFacts(false, false);
  for (const tour of catalog.tours) for (const fact of tour.requires) expect(facts).toHaveProperty(fact);
});

it('offers the conversation example only for an open routed work item', () => {
  expect(workspaceGuideFacts(true, true, { closedAt: null })).toMatchObject({ 'work-open': true, 'work-closed': false });
  expect(workspaceGuideFacts(true, true, { closedAt: 1 })).toMatchObject({ 'work-open': false, 'work-closed': true });
  expect(workspaceGuideFacts(true, true)).toMatchObject({ 'work-open': false, 'work-closed': false });
});
