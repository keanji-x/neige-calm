import { skipToken, useQuery } from '@tanstack/react-query';
import type { ReactNode } from 'react';
import type { ApiTransportPort } from '../../../../core/api/types.ts';
import type { UnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { readErrorText } from '../../../../core/domain/read-failure.ts';
import { templatePluginGuidesOperation } from '../../../../core/domain/template.ts';
import { TemplatePluginGuides as GuidePills } from '../../features/area/new-track/template-plugin-guides.tsx';
import { runOperation } from '../providers/queries.ts';

export function useTemplatePluginGuides(
  templateId: string | null, transport: ApiTransportPort, unauthorized: UnauthorizedChannel,
): ReactNode | undefined {
  const guides = useQuery({ queryKey: ['template-plugin-guides', templateId],
    enabled: templateId !== null,
    queryFn: templateId === null ? skipToken
      : ({ signal }) => runOperation(transport, { ...templatePluginGuidesOperation(templateId), signal }, unauthorized) });
  // ChatComposer reserves a header row for any React element, even one rendering null.
  if (templateId === null || (guides.error === null && (guides.data === undefined || guides.data.length === 0))) return undefined;
  return <GuidePills guides={guides.data} error={guides.error === null ? null : readErrorText(guides.error, 'Could not load template guides.')}
    onRetry={() => { void guides.refetch(); }} />;
}
