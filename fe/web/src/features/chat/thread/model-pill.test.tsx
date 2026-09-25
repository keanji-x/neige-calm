// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, within } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import {
  FOLLOW_INSTALLATION_DEFAULT,
  type ModelCatalog, type ModelSelection,
} from '../../../../../core/domain/conversation.ts';
import { ModelPill, type ModelGroup } from './model-pill.tsx';

afterEach(cleanup);

function model(overrides: Partial<ModelCatalog['models'][number]> = {}): ModelCatalog['models'][number] {
  return {
    id: 'preset-fast',
    model: 'gpt-5',
    display_name: 'GPT-5',
    description: 'The everyday one.',
    is_default: false,
    supported_reasoning_efforts: [
      { reasoning_effort: 'low', description: 'Answers sooner.' },
      { reasoning_effort: 'high', description: 'Thinks longer.' },
    ],
    default_reasoning_effort: 'low',
    ...overrides,
  };
}

function catalog(overrides: Partial<ModelCatalog> = {}): ModelCatalog {
  return {
    models: [model()],
    default: { model: 'gpt-5-codex', reasoning_effort: null },
    default_source: 'config_read',
    source: 'live',
    fetched_at_ms: 1_760_000_000_000,
    ...overrides,
  };
}

/** What `routes/models.rs` answers for a Claude Planner on a server that runs them (#1810). */
function claudeCatalog(overrides: Partial<ModelCatalog> = {}): ModelCatalog {
  return {
    models: ['opus', 'sonnet', 'haiku'].map((alias) => model({
      id: alias, model: alias, display_name: alias[0].toUpperCase() + alias.slice(1), description: `The ${alias} alias.`,
      supported_reasoning_efforts: [], default_reasoning_effort: null,
    })),
    default: { model: null, reasoning_effort: null },
    default_source: 'unknown',
    source: 'built_in',
    fetched_at_ms: null,
    ...overrides,
  };
}

function both(claude: ModelCatalog | null = claudeCatalog()): readonly ModelGroup[] {
  return [{ provider: 'codex', catalog: catalog() }, { provider: 'claude', catalog: claude }];
}

function trigger(name: RegExp): HTMLElement {
  return screen.getByRole('button', { name });
}

function openMenu(name: RegExp): HTMLElement {
  fireEvent.click(trigger(name));
  return screen.getByRole('menu');
}

describe('ModelPill', () => {
  it('names the default it is actually following, and only the name', () => {
    render(
      <ModelPill provider="codex" groups={[{ provider: 'codex', catalog: catalog() }]} selection={FOLLOW_INSTALLATION_DEFAULT} onChange={vi.fn()} />,
    );
    expect(trigger(/^Model:/).textContent).toBe('gpt-5-codex');
  });

  it('still says, to a screen reader, that it is following the default', () => {
    const { rerender } = render(
      <ModelPill provider="codex" groups={[{ provider: 'codex', catalog: catalog() }]} selection={FOLLOW_INSTALLATION_DEFAULT} onChange={vi.fn()} />,
    );
    expect(trigger(/^Model:/).getAttribute('aria-label'))
      .toBe("Model: gpt-5-codex (this installation's default)");

    rerender(
      <ModelPill provider="codex"
        groups={[{ provider: 'codex', catalog: catalog() }]}
        selection={{ model: 'gpt-5', reasoning_effort: null }}
        onChange={vi.fn()}
      />,
    );
    expect(trigger(/^Model:/).getAttribute('aria-label')).toBe('Model: GPT-5');
  });

  it('keeps the word in the menu, where it is the choice being offered', () => {
    render(
      <ModelPill provider="codex" groups={[{ provider: 'codex', catalog: catalog() }]} selection={FOLLOW_INSTALLATION_DEFAULT} onChange={vi.fn()} />,
    );
    const menu = openMenu(/^Model:/);
    expect(within(menu).getByText('Default (gpt-5-codex)')).toBeTruthy();
  });

  it('draws no chevron on the trigger', () => {
    render(
      <ModelPill provider="codex" groups={[{ provider: 'codex', catalog: catalog() }]} selection={FOLLOW_INSTALLATION_DEFAULT} onChange={vi.fn()} />,
    );
    expect(trigger(/^Model:/).querySelector('svg')).toBeNull();
  });

  it('will not name a default it was told it could not resolve', () => {
    render(
      <ModelPill provider="codex"
        groups={[{ provider: 'codex', catalog: catalog({ default_source: 'unknown', default: { model: 'gpt-5-global', reasoning_effort: null } }) }]}
        selection={FOLLOW_INSTALLATION_DEFAULT}
        onChange={vi.fn()}
      />,
    );
    expect(trigger(/^Model:/).textContent).toBe('Default');
    expect(screen.queryByText(/gpt-5-global/)).toBeNull();
  });

  it('shows the chosen model by its display name', () => {
    render(
      <ModelPill provider="codex"
        groups={[{ provider: 'codex', catalog: catalog() }]}
        selection={{ model: 'gpt-5', reasoning_effort: null }}
        onChange={vi.fn()}
      />,
    );
    expect(trigger(/^Model:/).textContent).toBe('GPT-5');
  });

  it('falls back to the slug for a model the catalog does not list', () => {
    render(
      <ModelPill provider="codex"
        groups={[{ provider: 'codex', catalog: catalog() }]}
        selection={{ model: 'gpt-6-preview', reasoning_effort: null }}
        onChange={vi.fn()}
      />,
    );
    expect(trigger(/^Model:/).textContent).toBe('gpt-6-preview');
  });

  it('hands back the slug and never the preset id', () => {
    const onChange = vi.fn();
    render(
      <ModelPill provider="codex" groups={[{ provider: 'codex', catalog: catalog() }]} selection={FOLLOW_INSTALLATION_DEFAULT} onChange={onChange} />,
    );
    fireEvent.click(within(openMenu(/^Model:/)).getByRole('menuitem', { name: /GPT-5/ }));
    expect(onChange).toHaveBeenCalledWith({ model: 'gpt-5', reasoning_effort: null }, 'codex');
    expect(onChange.mock.calls[0]?.[0]).not.toMatchObject({ model: 'preset-fast' });
  });

  it('drops the effort back to the default when the model changes', () => {
    const onChange = vi.fn();
    render(
      <ModelPill provider="codex"
        groups={[{ provider: 'codex', catalog: catalog() }]}
        selection={{ model: 'gpt-6-preview', reasoning_effort: 'high' }}
        onChange={onChange}
      />,
    );
    fireEvent.click(within(openMenu(/^Model:/)).getByRole('menuitem', { name: /GPT-5/ }));
    expect(onChange).toHaveBeenCalledWith({ model: 'gpt-5', reasoning_effort: null }, 'codex');
  });

  it('offers "Default" as a real choice and sends both nulls for it', () => {
    const onChange = vi.fn();
    render(
      <ModelPill provider="codex"
        groups={[{ provider: 'codex', catalog: catalog() }]}
        selection={{ model: 'gpt-5', reasoning_effort: 'high' }}
        onChange={onChange}
      />,
    );
    fireEvent.click(within(openMenu(/^Model:/)).getByRole('menuitem', { name: /^Default/ }));
    expect(onChange).toHaveBeenCalledWith({ model: null, reasoning_effort: null }, 'codex');
  });

  describe('the effort control', () => {
    it('offers the followed model’s efforts while no model is chosen', () => {
      render(
        <ModelPill provider="codex"
          groups={[{ provider: 'codex', catalog: catalog({ default: { model: 'gpt-5', reasoning_effort: 'low' } }) }]}
          selection={FOLLOW_INSTALLATION_DEFAULT}
          onChange={vi.fn()}
        />,
      );
      const effort = trigger(/^Reasoning effort:/);
      expect(effort.textContent).toBe('low');
      expect(effort.getAttribute('aria-label')).toBe('Reasoning effort: low (the default)');
      const menu = openMenu(/^Reasoning effort:/);
      expect(within(menu).getByText('Thinks longer.')).toBeTruthy();
    });

    it('sends the effort with a null model when the default is being followed', () => {
      const onChange = vi.fn();
      render(
        <ModelPill provider="codex"
          groups={[{ provider: 'codex', catalog: catalog({ default: { model: 'gpt-5', reasoning_effort: 'low' } }) }]}
          selection={FOLLOW_INSTALLATION_DEFAULT}
          onChange={onChange}
        />,
      );
      const menu = openMenu(/^Reasoning effort:/);
      fireEvent.click(within(menu).getByText('high'));
      expect(onChange).toHaveBeenCalledWith({ model: null, reasoning_effort: 'high' }, 'codex');
    });

    it('is absent when the followed model is not in the catalog', () => {
      render(
        <ModelPill provider="codex" groups={[{ provider: 'codex', catalog: catalog() }]} selection={FOLLOW_INSTALLATION_DEFAULT} onChange={vi.fn()} />,
      );
      expect(screen.queryByRole('button', { name: /^Reasoning effort:/ })).toBeNull();
    });

    it('is absent for a model that offers only one effort', () => {
      render(
        <ModelPill provider="codex"
          groups={[{ provider: 'codex', catalog: catalog({
            models: [model({ supported_reasoning_efforts: [{ reasoning_effort: 'medium', description: 'The only one.' }] })],
          }) }]}
          selection={{ model: 'gpt-5', reasoning_effort: null }}
          onChange={vi.fn()}
        />,
      );
      expect(screen.queryByRole('button', { name: /^Reasoning effort:/ })).toBeNull();
    });

    it('offers the chosen model\'s efforts with codex\'s own descriptions', () => {
      render(
        <ModelPill provider="codex"
          groups={[{ provider: 'codex', catalog: catalog() }]}
          selection={{ model: 'gpt-5', reasoning_effort: 'high' }}
          onChange={vi.fn()}
        />,
      );
      expect(trigger(/^Reasoning effort:/).textContent).toBe('high');
      const menu = openMenu(/^Reasoning effort:/);
      expect(within(menu).getByText('Thinks longer.')).toBeTruthy();
      expect(within(menu).getByText('Answers sooner.')).toBeTruthy();
    });

    it('keeps the model when only the effort changes', () => {
      const onChange = vi.fn();
      render(
        <ModelPill provider="codex"
          groups={[{ provider: 'codex', catalog: catalog() }]}
          selection={{ model: 'gpt-5', reasoning_effort: null }}
          onChange={onChange}
        />,
      );
      fireEvent.click(within(openMenu(/^Reasoning effort:/)).getByRole('menuitem', { name: /high/ }));
      expect(onChange).toHaveBeenCalledWith({ model: 'gpt-5', reasoning_effort: 'high' }, 'codex');
    });
  });

  it('keeps effort selection in the model menu when its host asks for one control', () => {
    const onChange = vi.fn();
    const view = render(<ModelPill provider="codex" groups={[{ provider: 'codex', catalog: catalog() }]}
      selection={{ model: 'gpt-5', reasoning_effort: null }}
      effortControl="in-menu" onChange={onChange} />);
    expect(screen.queryByRole('button', { name: /^Reasoning effort:/ })).toBeNull();
    const efforts = within(openMenu(/^Model:/)).getByRole('group', { name: 'Reasoning effort' });
    fireEvent.click(within(efforts).getByRole('menuitem', { name: /high/ }));
    expect(onChange).toHaveBeenLastCalledWith({ model: 'gpt-5', reasoning_effort: 'high' }, 'codex');
    view.rerender(<ModelPill provider="codex" groups={[{ provider: 'codex', catalog: catalog() }]}
      selection={{ model: 'gpt-5', reasoning_effort: 'high' }}
      effortControl="in-menu" onChange={onChange} />);
    // Reopen through the keyboard; DropdownMenu guards immediate repeat pointer clicks.
    fireEvent.keyDown(trigger(/^Model:/), { key: 'ArrowDown' });
    const chosenEfforts = within(screen.getByRole('menu')).getByRole('group', { name: 'Reasoning effort' });
    expect(within(chosenEfforts).getByRole('menuitem', { name: /high/ }).textContent).toContain('Selected');
    fireEvent.click(within(chosenEfforts).getByRole('menuitem', { name: /^Default/ }));
    expect(onChange).toHaveBeenLastCalledWith({ model: 'gpt-5', reasoning_effort: null }, 'codex');
  });

  it('separates a daemon that could not be asked from an account with nothing to offer', () => {
    const { unmount } = render(
      <ModelPill provider="codex"
        groups={[{ provider: 'codex', catalog: catalog({ models: [], source: 'unavailable', default_source: 'config_toml' }) }]}
        selection={FOLLOW_INSTALLATION_DEFAULT}
        onChange={vi.fn()}
      />,
    );
    const unreachable = trigger(/^Model:/);
    expect(unreachable.hasAttribute('disabled') || unreachable.getAttribute('aria-disabled') === 'true')
      .toBe(true);
    unmount();

    render(
      <ModelPill provider="codex"
        groups={[{ provider: 'codex', catalog: catalog({ models: [], source: 'live' }) }]}
        selection={FOLLOW_INSTALLATION_DEFAULT}
        onChange={vi.fn()}
      />,
    );
    const live = trigger(/^Model:/);
    expect(live.hasAttribute('disabled') || live.getAttribute('aria-disabled') === 'true').toBe(false);
    expect(within(openMenu(/^Model:/)).getByText(/No models available on this account/)).toBeTruthy();
  });

  it('says why an unavailable catalog is empty in the words of its provider', () => {
    const unavailable = catalog({ models: [], source: 'unavailable', default_source: 'unknown' });
    const { unmount } = render(<ModelPill provider="codex" groups={[{ provider: 'codex', catalog: unavailable }]} selection={FOLLOW_INSTALLATION_DEFAULT} onChange={vi.fn()} />);
    /* The menu cannot open while unavailable, but its rows are rendered; the text is what a Codex reader is told. */
    expect(document.body.textContent).toContain('codex is not running');
    unmount();
    render(<ModelPill provider="claude" groups={[{ provider: 'claude', catalog: unavailable }]} selection={FOLLOW_INSTALLATION_DEFAULT} onChange={vi.fn()} />);
    expect(document.body.textContent).toContain('This server does not run Claude Planners');
    expect(document.body.textContent).not.toContain('codex is not running');
  });

  it('explains the compaction risk once, as a note rather than a per-switch warning', () => {
    render(
      <ModelPill provider="codex" groups={[{ provider: 'codex', catalog: catalog() }]} selection={FOLLOW_INSTALLATION_DEFAULT} onChange={vi.fn()} />,
    );
    const note = within(openMenu(/^Model:/)).getByRole('note');
    expect(note.textContent).toContain('smaller context window');
    expect(screen.queryByRole('menuitem', { name: /smaller context window/ })).toBeNull();
  });

  it('shows the default and stays quiet while the catalog is still loading', () => {
    render(
      <ModelPill provider="codex" groups={[{ provider: 'codex', catalog: null }]} selection={FOLLOW_INSTALLATION_DEFAULT} onChange={vi.fn()} />,
    );
    expect(trigger(/^Model:/).textContent).toBe('Default');
    expect(screen.queryByRole('button', { name: /^Reasoning effort:/ })).toBeNull();
  });

  it('is unavailable while its host says so', () => {
    const selection: ModelSelection = { model: 'gpt-5', reasoning_effort: 'high' };
    render(
      <ModelPill provider="codex" groups={[{ provider: 'codex', catalog: catalog() }]} selection={selection} onChange={vi.fn()} isDisabled />,
    );
    for (const name of [/^Model:/, /^Reasoning effort:/] as const) {
      const button = trigger(name);
      expect(button.hasAttribute('disabled') || button.getAttribute('aria-disabled') === 'true')
        .toBe(true);
    }
  });

  describe('grouped by provider (#1810)', () => {
    it('leaves the Claude group out while the server has no Claude Planner or has not answered', () => {
      for (const claude of [claudeCatalog({ models: [], source: 'unavailable' }), null]) {
        const view = render(<ModelPill groups={both(claude)} provider="codex" selection={FOLLOW_INSTALLATION_DEFAULT} onChange={vi.fn()} />);
        expect(trigger(/^Model:/).textContent).toBe('gpt-5-codex');
        const menu = openMenu(/^Model:/);
        expect(within(menu).queryByRole('group', { name: 'Claude' })).toBeNull();
        expect(within(menu).queryByRole('menuitem', { name: /Sonnet/ })).toBeNull();
        view.unmount();
      }
    });

    it('offers each provider as a group, Default first, when the server runs Claude Planners', () => {
      render(<ModelPill groups={both()} provider="codex" selection={FOLLOW_INSTALLATION_DEFAULT} onChange={vi.fn()} />);
      const menu = openMenu(/^Model:/);
      const codex = within(menu).getByRole('group', { name: 'Codex' });
      expect(within(codex).getAllByRole('menuitem').map((item) => item.textContent))
        .toEqual(['Default (gpt-5-codex)Selected', 'GPT-5']);
      const claude = within(menu).getByRole('group', { name: 'Claude' });
      expect(within(claude).getAllByRole('menuitem').map((item) => item.textContent))
        .toEqual(['Default', 'Opus', 'Sonnet', 'Haiku']);
    });

    it('hands back the Claude alias and the provider with a Claude pick, and codex with a Codex one', () => {
      const onChange = vi.fn();
      const view = render(<ModelPill groups={both()} provider="codex" selection={{ model: 'gpt-5', reasoning_effort: 'high' }} onChange={onChange} />);
      fireEvent.click(within(within(openMenu(/^Model:/)).getByRole('group', { name: 'Claude' })).getByRole('menuitem', { name: 'Sonnet' }));
      expect(onChange).toHaveBeenLastCalledWith({ model: 'sonnet', reasoning_effort: null }, 'claude');
      view.rerender(<ModelPill groups={both()} provider="claude" selection={{ model: 'sonnet', reasoning_effort: null }} onChange={onChange} />);
      fireEvent.keyDown(trigger(/^Model:/), { key: 'ArrowDown' });
      const menu = screen.getByRole('menu');
      expect(within(within(menu).getByRole('group', { name: 'Claude' })).getByRole('menuitem', { name: /Sonnet/ }).textContent).toContain('Selected');
      expect(within(within(menu).getByRole('group', { name: 'Codex' })).getByRole('menuitem', { name: /^Default/ }).textContent).not.toContain('Selected');
      fireEvent.click(within(within(menu).getByRole('group', { name: 'Codex' })).getByRole('menuitem', { name: /^Default/ }));
      expect(onChange).toHaveBeenLastCalledWith({ model: null, reasoning_effort: null }, 'codex');
    });

    it('names the provider on the trigger when more than one is offered', () => {
      const view = render(<ModelPill groups={both()} provider="claude" selection={{ model: 'sonnet', reasoning_effort: null }} onChange={vi.fn()} />);
      expect(trigger(/^Model:/).textContent).toBe('Claude Sonnet');
      view.rerender(<ModelPill groups={both()} provider="claude" selection={FOLLOW_INSTALLATION_DEFAULT} onChange={vi.fn()} />);
      expect(trigger(/^Model:/).getAttribute('aria-label')).toBe('Model: Claude Default');
      view.rerender(<ModelPill groups={both()} provider="codex" selection={FOLLOW_INSTALLATION_DEFAULT} onChange={vi.fn()} />);
      expect(trigger(/^Model:/).getAttribute('aria-label')).toBe("Model: Codex gpt-5-codex (this installation's default)");
    });

    it('offers no reasoning effort for Claude, even for an entry that lists some', () => {
      const listed = claudeCatalog({ models: [model({ id: 'opus', model: 'opus', display_name: 'Opus' })] });
      for (const effortControl of ['separate', 'in-menu'] as const) {
        const view = render(<ModelPill groups={[{ provider: 'claude', catalog: listed }]} provider="claude"
          selection={{ model: 'opus', reasoning_effort: null }} effortControl={effortControl} onChange={vi.fn()} />);
        expect(screen.queryByRole('button', { name: /^Reasoning effort:/ })).toBeNull();
        expect(within(openMenu(/^Model:/)).queryByRole('group', { name: 'Reasoning effort' })).toBeNull();
        view.unmount();
      }
    });

    it('inside a Claude track lists only the Claude aliases, without headings or the codex note', () => {
      render(<ModelPill groups={[{ provider: 'claude', catalog: claudeCatalog() }]} provider="claude"
        selection={{ model: 'haiku', reasoning_effort: null }} onChange={vi.fn()} />);
      expect(trigger(/^Model:/).textContent).toBe('Haiku');
      const menu = openMenu(/^Model:/);
      expect(within(menu).getAllByRole('menuitem').map((item) => item.textContent))
        .toEqual(['Default', 'Opus', 'Sonnet', 'HaikuSelected']);
      expect(within(menu).queryByRole('group')).toBeNull();
      expect(within(menu).queryByRole('note')).toBeNull();
    });

    it('keeps Claude choosable while codex is not running', () => {
      render(<ModelPill groups={[{ provider: 'codex', catalog: catalog({ models: [], source: 'unavailable' }) },
        { provider: 'claude', catalog: claudeCatalog() }]} provider="codex" selection={FOLLOW_INSTALLATION_DEFAULT} onChange={vi.fn()} />);
      const menu = openMenu(/^Model:/);
      expect(within(within(menu).getByRole('group', { name: 'Codex' })).getByText('codex is not running')).toBeTruthy();
      expect(within(within(menu).getByRole('group', { name: 'Claude' })).getByRole('menuitem', { name: 'Opus' })).toBeTruthy();
    });
  });
});
