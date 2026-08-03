import { describe, expectTypeOf, it, vi } from 'vitest';
import type { ReactNode } from 'react';
import { Dialog, ConfirmDialog, useDialogView, type DialogViewController } from './dialog/public.tsx';
import { Menu, type MenuTriggerProps } from './menu/public.tsx';
import { useRovingTabindex, type RovingResult } from './focus/public.ts';
import { DirectoryBrowser, type ListDirectory } from './directory-browser/public.tsx';
import { DirectoryField } from './schema-form/fields/DirectoryField/public.tsx';

const listDirectory: ListDirectory = () => Promise.resolve({ path: '/workspace', parent: '/', entries: [] });

function FakeConsumer() {
  const view: DialogViewController | null = useDialogView();
  const roving: RovingResult<HTMLButtonElement> = useRovingTabindex({ itemCount: 1 });
  return <>
    <button {...roving.getItemProps(0)}>Focusable</button>
    <Menu items={[{ label: 'Create', onSelect: vi.fn() }]} trigger={(props: MenuTriggerProps) => <button {...props}>Open</button>}/>
    <Dialog open title="Primitive consumer" onClose={vi.fn()}><button onClick={() => view?.pushView({ title: 'Child', body: 'Body' })}>Push</button></Dialog>
    <ConfirmDialog open title="Confirm action" onConfirm={vi.fn()} onCancel={vi.fn()}/>
    <DirectoryBrowser listDirectory={listDirectory} initialPath={null} onCancel={vi.fn()} onSelect={vi.fn()}/>
    <DirectoryField listDirectory={listDirectory} value="" onChange={vi.fn()}/>
  </>;
}

describe('minimal public-only consumer', () => {
  it('[type-only] forms a compilable consumer chain for every frozen primitive', () => {
    expectTypeOf(FakeConsumer).returns.toMatchTypeOf<ReactNode>();
  });
});
