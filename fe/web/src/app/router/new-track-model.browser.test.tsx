// Exercise the app's composition of the Area form and Chat model control
// with the production styles and a real narrow-screen browser layout.
import '../../styles/entry.css';
import { render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { expect, it, vi } from 'vitest';
import { page } from 'vitest/browser';
import type { TrackTemplate } from '../../../../core/domain/track.ts';
import { FOLLOW_INSTALLATION_DEFAULT, type ModelCatalog, type ModelSelection } from '../../../../core/domain/conversation.ts';
import type { ListDirectory } from '../../ui/directory-browser/public.tsx';
import { NewTrackForm } from '../../features/area/new-track/public.tsx';
import { ModelPill } from '../../features/chat/thread/model-pill.tsx';
import { useState } from '../../ui/state/public.ts';

function ModelForm({
  onChange = () => undefined, submitting = false, locked = false, initialCwd = null,
  templates = [], initialTemplateId = null, modelName = 'Test model',
  listDirectory = (path) => Promise.resolve({ path: path ?? '/', parent: null, entries: [] }),
}: Readonly<{
  onChange?: (selection: ModelSelection) => void;
  submitting?: boolean;
  locked?: boolean;
  initialCwd?: string | null;
  templates?: readonly TrackTemplate[];
  initialTemplateId?: string | null;
  modelName?: string;
  listDirectory?: ListDirectory;
}>) {
  const [selection, setSelection] = useState<ModelSelection>(FOLLOW_INSTALLATION_DEFAULT);
  const catalog: ModelCatalog = {
    models: [{ id: 'test-model', model: 'test-model', display_name: modelName, description: '',
      is_default: true, default_reasoning_effort: 'low', supported_reasoning_efforts: [
        { reasoning_effort: 'low', description: 'Faster' },
        { reasoning_effort: 'high', description: 'More reasoning' },
      ] }],
    default: { model: null, reasoning_effort: null }, default_source: 'unknown',
    source: 'live', fetched_at_ms: 1,
  };
  return <NewTrackForm submitting={submitting} locked={locked} error={null} templates={templates} templatesLoaded
    initialTemplateId={initialTemplateId} initialCwd={initialCwd} onManageRecipes={vi.fn()}
    listDirectory={listDirectory} onSubmit={vi.fn()}
    modelControls={<ModelPill provider="codex" groups={[{ provider: 'codex', catalog }]} selection={selection} effortControl="in-menu" onChange={(next) => { setSelection(next); onChange(next); }} />}
  />;
}

function expectFullLabel(name: string) {
  const label = [...screen.getByRole('button', { name }).querySelectorAll('span')]
    .find((element) => getComputedStyle(element).textOverflow === 'ellipsis');
  if (label === undefined) throw new Error(`Missing visible label for ${name}`);
  expect(label.scrollWidth).toBeLessThanOrEqual(label.clientWidth);
}

it('keeps three phone preferences while selecting model and effort in one menu', async () => {
  await page.viewport(390, 844);
  const onChange = vi.fn();
  const view = render(<ModelForm onChange={onChange} />);
  try {
    await userEvent.click(screen.getByRole('button', { name: 'Model: Default' }));
    await userEvent.click(screen.getByRole('menuitem', { name: 'Test model' }));
    // Reopen the trigger through the keyboard contract, away from the pointer guard.
    screen.getByRole('button', { name: 'Model: Test model' }).focus();
    await userEvent.keyboard('{ArrowDown}');
    await userEvent.click(screen.getByRole('menuitem', { name: /high/ }));
    expect(onChange).toHaveBeenLastCalledWith({ model: 'test-model', reasoning_effort: 'high' });
    expect(screen.queryByRole('button', { name: /^Reasoning effort:/ })).toBeNull();
    expect(screen.getAllByRole('button')).toHaveLength(3); // Options, Model and Create track.
    for (const [width, height] of [[390, 844], [320, 700]]) {
      await page.viewport(width, height);
      expect(await screen.findByRole('button', { name: 'Track options' })).toBeTruthy();
      const model = screen.getByRole('button', { name: 'Model: Test model' }).getBoundingClientRect();
      const send = screen.getByRole('button', { name: 'Create track' }).getBoundingClientRect();
      expect(Math.abs(model.top + model.height / 2 - send.top - send.height / 2)).toBeLessThan(2);
      expect(model.right).toBeLessThanOrEqual(send.left);
      expect(send.right).toBeLessThanOrEqual(window.innerWidth);
      expect(document.documentElement.scrollWidth).toBe(window.innerWidth);
    }
  } finally {
    view.unmount();
    await page.viewport(1280, 720);
  }
});

it('gives the phone composer a compact input and one aligned preference row without overflow', async () => {
  const view = render(<ModelForm />);
  try {
    for (const [width, height] of [[390, 844], [320, 700], [844, 390]]) {
      await page.viewport(width, height);
      const heading = screen.getByRole('heading', { level: 1 });
      const input = screen.getByRole('textbox', { name: 'What this track should do' });
      const model = screen.getByRole('button', { name: 'Model: Default' }).getBoundingClientRect();
      const send = screen.getByRole('button', { name: 'Create track' }).getBoundingClientRect();
      if (width < 600) {
        expect(await screen.findByRole('button', { name: 'Track options' })).toBeTruthy();
        expect(screen.queryByRole('button', { name: 'Template: No template' })).toBeNull();
      } else {
        await waitFor(() => { expect(screen.queryByRole('button', { name: 'Track options' })).toBeNull(); });
        for (const name of ['Template: No template', 'Folder: Neige workspace', 'Model: Default']) expectFullLabel(name);
      }
      expect(Math.abs(model.top + model.height / 2 - send.top - send.height / 2)).toBeLessThan(2);
      expect(model.right).toBeLessThanOrEqual(send.left);
      expect(model.bottom).toBeLessThanOrEqual(height);
      expect(input.getBoundingClientRect().height).toBeLessThan(66);
      expect(parseFloat(getComputedStyle(input).fontSize)).toBeGreaterThanOrEqual(18);
      expect(parseFloat(getComputedStyle(heading).fontSize)).toBeGreaterThanOrEqual(22);
      expect(document.documentElement.scrollWidth).toBe(width);
      expect(getComputedStyle(heading).fontFamily).toBe(getComputedStyle(screen.getByRole('button', { name: 'Create track' })).fontFamily);
    }
  } finally {
    view.unmount();
    await page.viewport(1280, 720);
  }
});

it('keeps a retained phone draft at the same text size and height while submitting or locked', async () => {
  await page.viewport(320, 740);
  const view = render(<ModelForm />);
  try {
    const input = screen.getByRole('textbox', { name: 'What this track should do' });
    await userEvent.type(input, '这个输入框在等待创建结果时，字体和换行应该保持完全一致。');
    const originalText = input.textContent;
    const originalStyle = getComputedStyle(input);
    const fontSize = originalStyle.fontSize;
    const lineHeight = originalStyle.lineHeight;
    const height = input.getBoundingClientRect().height;
    for (const [submitting, locked] of [[true, false], [false, true], [false, false]]) {
      view.rerender(<ModelForm submitting={submitting} locked={locked} />);
      expect(input.textContent).toBe(originalText);
      expect(screen.getByRole<HTMLButtonElement>('button', { name: 'Track options' }).disabled).toBe(submitting || locked);
      expect(getComputedStyle(input).fontSize).toBe(fontSize);
      expect(getComputedStyle(input).lineHeight).toBe(lineHeight);
      expect(input.getBoundingClientRect().height).toBe(height);
    }
  } finally {
    view.unmount();
    await page.viewport(1280, 720);
  }
});

it('keeps long custom preferences readable inside overflow and preserves the folder clear action', async () => {
  const templateTitle = 'A long custom template for checking the mobile preference strip';
  const modelName = 'A long custom model for careful reasoning';
  const cwd = '/srv/projects/a-long-custom-folder-for-the-mobile-layout';
  const view = render(<ModelForm initialCwd={cwd} initialTemplateId="long-template"
    templates={[{ id: 'long-template', title: templateTitle, tasks: [] }]} modelName={modelName} />);
  try {
    await page.viewport(320, 740);
    await userEvent.click(screen.getByRole('button', { name: 'Model: Default' }));
    await userEvent.click(screen.getByRole('menuitem', { name: modelName }));
    await userEvent.click(await screen.findByRole('button', { name: 'Track options' }));
    const options = await screen.findByRole('dialog', { name: 'Track options' });
    for (const width of [320, 390]) {
      await page.viewport(width, 740);
      const folder = within(options).getByRole('button', { name: `Folder: ${cwd}` }).getBoundingClientRect();
      const clear = within(options).getByRole('button', { name: 'Use a Neige workspace instead' }).getBoundingClientRect();
      expect(folder.right).toBeLessThanOrEqual(clear.left);
      expect(clear.right).toBeLessThanOrEqual(width);
      expect(document.documentElement.scrollWidth).toBe(width);
    }
    await userEvent.click(within(options).getByRole('button', { name: 'Use a Neige workspace instead' }));
    expect(screen.getByRole('button', { name: 'Folder: Neige workspace' })).toBeTruthy();
    expect(screen.queryByRole('button', { name: 'Use a Neige workspace instead' })).toBeNull();
  } finally {
    view.unmount();
    await page.viewport(1280, 720);
  }
});

it('keeps Model beside Create and moves preferences into overflow before they stop fitting', async () => {
  const view = render(<ModelForm />);
  try {
    await page.viewport(390, 844);
    const model = screen.getByRole('button', { name: 'Model: Default' }).getBoundingClientRect();
    const send = screen.getByRole('button', { name: 'Create track' }).getBoundingClientRect();
    expect(Math.abs(model.top + model.height / 2 - send.top - send.height / 2)).toBeLessThan(2);
    expect(await screen.findByRole('button', { name: 'Track options' })).toBeTruthy();
    await page.viewport(320, 740);
    expect(await screen.findByRole('button', { name: 'Track options' })).toBeTruthy();
    expect(screen.queryByRole('button', { name: 'Template: No template' })).toBeNull();
    expect(screen.queryByRole('button', { name: 'Folder: Neige workspace' })).toBeNull();
    await page.viewport(600, 740);
    await waitFor(() => { expect(screen.queryByRole('button', { name: 'Track options' })).toBeNull(); });
    expectFullLabel('Template: No template');
    expectFullLabel('Folder: Neige workspace');
  } finally {
    view.unmount();
    await page.viewport(1280, 720);
  }
});

it('dismisses the nested template picker before overflow and leaves directory selection usable', async () => {
  // Astryx ignores repeat trigger clicks for 50 ms after a layer closes.
  const user = userEvent.setup({ delay: 60 });
  await page.viewport(320, 740);
  const cwd = '/srv/a-long-directory-selected-through-the-real-browser';
  const view = render(<ModelForm templates={[{ id: 'small', title: 'Small change', tasks: [{ key: 'inspect', goal: 'Read the change.' }] }]}
    listDirectory={() => Promise.resolve({ path: cwd, parent: null, entries: [] })} />);
  try {
    const more = await screen.findByRole('button', { name: 'Track options' });
    more.focus();
    await user.keyboard('{Enter}');
    const options = await screen.findByRole('dialog', { name: 'Track options' });
    await user.click(within(options).getByRole('button', { name: 'Template: No template' }));
    expect(await screen.findByRole('menu', { name: 'Template: No template' })).toBeTruthy();
    await user.keyboard('{Escape}');
    await waitFor(() => { expect(screen.queryByRole('menu')).toBeNull(); });
    expect(screen.getByRole('dialog', { name: 'Track options' })).toBeTruthy();
    await user.click(within(options).getByRole('button', { name: 'Template: No template' }));
    // Native HoverCard previews use the first touch; keyboard activation selects
    // the same real menu item without depending on that baseline pointer behavior.
    (await screen.findByRole('menuitem', { name: /^Small change/ })).focus();
    await user.keyboard('{Enter}');
    expect(await screen.findByRole('button', { name: 'Template: Small change' })).toBeTruthy();
    await user.click(within(options).getByRole('button', { name: 'Template: Small change' }));
    await user.click(await screen.findByRole('menuitem', { name: 'No template' }));
    await user.click(within(options).getByRole('button', { name: 'Folder: Neige workspace' }));
    const cancelledDirectory = await screen.findByRole('dialog', { name: 'Choose a directory' });
    await user.click(within(cancelledDirectory).getByRole('button', { name: 'Cancel' }));
    await waitFor(() => { expect(screen.queryByRole('dialog', { name: 'Choose a directory' })).toBeNull(); });
    expect(document.activeElement).toBe(screen.getByRole('button', { name: 'Folder: Neige workspace' }));
    await user.click(screen.getByRole('button', { name: 'Folder: Neige workspace' }));
    const directory = await screen.findByRole('dialog', { name: 'Choose a directory' });
    await user.click(await within(directory).findByRole('button', { name: 'Select this directory' }));
    await waitFor(() => { expect(screen.queryByRole('dialog', { name: 'Choose a directory' })).toBeNull(); });
    if (screen.queryByRole('dialog', { name: 'Track options' }) === null) {
      await user.click(screen.getByRole('button', { name: 'Track options' }));
    }
    expect(await screen.findByRole('button', { name: `Folder: ${cwd}` })).toBeTruthy();
    await user.keyboard('{Escape}');
    await waitFor(() => { expect(screen.queryByRole('dialog', { name: 'Track options' })).toBeNull(); });
    expect(document.activeElement).toBe(screen.getByRole('button', { name: 'Track options' }));
    await page.viewport(600, 844);
    await user.click(screen.getByRole('button', { name: 'Track options' }));
    await user.click(await screen.findByRole('button', { name: 'Use a Neige workspace instead' }));
    await waitFor(() => { expect(screen.queryByRole('button', { name: 'Track options' })).toBeNull(); });
    expect(screen.getByRole('button', { name: 'Folder: Neige workspace' })).toBeTruthy();
    expectFullLabel('Template: No template');
    expectFullLabel('Folder: Neige workspace');
  } finally {
    view.unmount();
    await page.viewport(1280, 720);
  }
});

it('restores the visible options trigger after a trusted pointer cancels directory browsing', async () => {
  await page.viewport(320, 740);
  const view = render(<ModelForm />);
  try {
    await page.getByRole('button', { name: 'Track options', exact: true }).click();
    await page.getByRole('button', { name: 'Folder: Neige workspace', exact: true }).click();
    await page.getByRole('dialog', { name: 'Choose a directory', exact: true }).getByRole('button', { name: 'Cancel', exact: true }).click();
    await waitFor(() => { expect(screen.queryByRole('dialog', { name: 'Choose a directory' })).toBeNull(); });
    expect(screen.queryByRole('dialog', { name: 'Track options' })).toBeNull();
    await waitFor(() => { expect(document.activeElement).toBe(screen.getByRole('button', { name: 'Track options' })); });
  } finally {
    view.unmount();
    await page.viewport(1280, 720);
  }
});

it('keeps standard secondary button affordances on the visible model and overflow controls', async () => {
  await page.viewport(320, 740);
  const view = render(<ModelForm />);
  try {
    const more = await screen.findByRole('button', { name: 'Track options' });
    const model = screen.getByRole('button', { name: 'Model: Default' });
    expect(more.getAttribute('data-variant')).toBe('secondary');
    expect(model.getAttribute('data-variant')).toBe('secondary');
    expect(getComputedStyle(model).backgroundColor).not.toBe('rgba(0, 0, 0, 0)');
    expect(parseFloat(getComputedStyle(model).paddingInlineStart)).toBeGreaterThan(0);
  } finally {
    view.unmount();
    await page.viewport(1280, 720);
  }
});

it('restores a focused overflow trigger into inline preferences without moving unrelated focus on resize', async () => {
  await page.viewport(320, 740);
  const view = render(<ModelForm />);
  try {
    const more = await screen.findByRole('button', { name: 'Track options' });
    more.focus();
    expect(document.activeElement).toBe(more);
    await page.viewport(844, 740);
    const folder = await screen.findByRole('button', { name: 'Folder: Neige workspace' });
    await waitFor(() => { expect(document.activeElement).toBe(folder); });

    const editor = screen.getByRole('textbox', { name: 'What this track should do' });
    editor.focus();
    await page.viewport(320, 740);
    expect(await screen.findByRole('button', { name: 'Track options' })).toBeTruthy();
    expect(document.activeElement).toBe(editor);

    const model = screen.getByRole('button', { name: 'Model: Default' });
    model.focus();
    await page.viewport(844, 740);
    expect(await screen.findByRole('button', { name: 'Folder: Neige workspace' })).toBeTruthy();
    expect(document.activeElement).toBe(model);
  } finally {
    view.unmount();
    await page.viewport(1280, 720);
  }
});


it('uses the card radius for the composer while retaining full pill and send contours', async () => {
  await page.viewport(390, 844);
  const view = render(<ModelForm />);
  try {
    const input = screen.getByRole('textbox', { name: 'What this track should do' });
    const body = input.closest('[data-density]')!.firstElementChild!;
    expect(getComputedStyle(body).borderRadius).toBe('16px');
    input.blur();
    await expect.poll(() => getComputedStyle(body).boxShadow).toBe('none');
    await page.elementLocator(body).hover();
    expect(getComputedStyle(body).boxShadow).toBe('none');
    await page.getByRole('textbox', { name: 'What this track should do' }).click();
    await expect.poll(() => getComputedStyle(body).boxShadow).toContain('2px inset');
    expect(getComputedStyle(body).borderRadius).toBe('16px');

    for (const name of ['Track options', 'Model: Default', 'Create track']) {
      expect(getComputedStyle(await screen.findByRole('button', { name })).borderRadius).toBe('999px');
    }
    await page.getByRole('button', { name: 'Track options', exact: true }).click();
    for (const name of ['Template: No template', 'Folder: Neige workspace']) {
      expect(getComputedStyle(await screen.findByRole('button', { name })).borderRadius).toBe('999px');
    }

    await page.getByRole('button', { name: 'Template: No template', exact: true }).click();
    const templateMenu = await page.getByRole('menu').findElement();
    expect(getComputedStyle(templateMenu).borderRadius).toBe('16px');
    expect(getComputedStyle(templateMenu).paddingTop).toBe('4px');
    const templateItem = await page.getByRole('menuitem', { name: /^No template/ }).findElement();
    expect(getComputedStyle(templateItem).borderRadius).toBe('12px');
    await page.getByRole('menuitem', { name: /^No template/ }).click();
    await page.getByRole('button', { name: 'Model: Default', exact: true }).click();
    const modelMenu = await page.getByRole('menu').findElement();
    expect(getComputedStyle(modelMenu).borderRadius).toBe('16px');
    expect(getComputedStyle(modelMenu).paddingTop).toBe('4px');
    expect(getComputedStyle(await page.getByRole('menuitem', { name: 'Test model', exact: true }).findElement()).borderRadius).toBe('12px');
    expect(getComputedStyle(document.documentElement).getPropertyValue('-webkit-tap-highlight-color')).toBe('rgba(0, 0, 0, 0)');
  } finally {
    view.unmount();
    await page.viewport(1280, 720);
  }
});
