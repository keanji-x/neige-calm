import { Banner } from '@astryxdesign/core/Banner';
import { Button } from '@astryxdesign/core/Button';
import { useEffect } from 'react';

import type { RecipePreview as SavedPreview } from '../../../../../core/domain/recipe-preview.ts';
import type { TrackRecipe } from '../../../../../core/domain/track.ts';
import { useState } from '../../../ui/state/public.ts';
import { ReportDocument } from '../document/public.tsx';

export type RecipePreviewOutcome =
  | Readonly<{ kind: 'ready'; preview: SavedPreview }>
  | Readonly<{ kind: 'conflict' }>
  | Readonly<{ kind: 'failed'; message: string }>;
export type RecipePreviewLoader = (id: string, revision: number, signal: AbortSignal) => Promise<RecipePreviewOutcome>;

export function RecipePreview({ recipe, load }: { recipe: TrackRecipe; load: RecipePreviewLoader }) {
  const [state, setState] = useState<RecipePreviewOutcome | { kind: 'loading' }>({ kind: 'loading' });
  const [attempt, setAttempt] = useState(0);
  useEffect(() => {
    const controller = new AbortController();
    setState({ kind: 'loading' });
    void (async () => {
      try {
        const result = await load(recipe.id, recipe.revision, controller.signal);
        if (!controller.signal.aborted) setState(result);
      } catch (error) {
        if (!controller.signal.aborted) setState({ kind: 'failed', message: error instanceof Error ? error.message : '无法读取模板预览。' });
      }
    })();
    return () => { controller.abort(); };
  }, [recipe.id, recipe.revision, load, attempt]);

  if (state.kind === 'loading') return <p role="status">正在加载已保存的模板…</p>;
  const mismatch = state.kind === 'ready' && (state.preview.id !== recipe.id
    || state.preview.revision !== recipe.revision || state.preview.report.summary !== recipe.title.trim());
  if (state.kind === 'conflict' || mismatch) return <div>
    <Banner status="warning" title="模板已在其他位置更新，请重试读取最新版本。"/>
    <Button type="button" variant="secondary" size="sm" label="重试预览" onClick={() => setAttempt(attempt + 1)}/>
  </div>;
  if (state.kind === 'failed') return <div>
    <Banner status="error" title={state.message}/>
    <Button type="button" variant="secondary" size="sm" label="重试预览" onClick={() => setAttempt(attempt + 1)}/>
  </div>;
  const report = state.preview.report;
  if (report.body === '' && report.blocks?.length === 0) return <p>模板尚无正文，可通过 Edit 添加内容。</p>;
  return <>
    <p>模板结构预览；实时数据在 Track 中显示。</p>
    <ReportDocument report={report} empty={null} mode="preview"/>
  </>;
}
