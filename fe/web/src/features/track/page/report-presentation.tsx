import { Button } from '@astryxdesign/core/Button';
import { getIcon } from '@astryxdesign/core/Icon';
import type { Ref } from 'react';
import type { ReportPresentation } from '../../../../../core/domain/report.ts';
import { useState } from '../../../ui/state/public.ts';

/** A user choice wins over report refreshes, but never crosses track identity. */
export function useReportPanel(trackId: string, presentation: ReportPresentation) {
  const [choice, setChoice] = useState<Readonly<{ trackId: string; open: boolean }> | null>(null);
  const open = choice?.trackId === trackId ? choice.open : presentation === 'document';
  return { open, toggle: () => setChoice({ trackId, open: !open }) };
}

export function ReportPanelToggle({ open, onToggle, buttonRef }: {
  open: boolean; onToggle: () => void; buttonRef: Ref<HTMLButtonElement>;
}) {
  return <Button label={open ? 'Hide track panel' : 'Show track panel'}
    tooltip="Cards, tasks and conversations" icon={getIcon('viewColumns')}
    isIconOnly variant="ghost" size="sm" aria-expanded={open}
    aria-controls="mobile-track-panel" onClick={onToggle} ref={buttonRef} />;
}
