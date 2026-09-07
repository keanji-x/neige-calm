// @vitest-environment jsdom
//
// #1505 S4-3 — the model picker's own contract, away from the router.
//
// Everything here is about what the reader is told and what the caller is
// handed. The rule that decides which model a *turn* runs with lives in the
// kernel (`crates/calm-server/src/planner_model.rs`) and is pinned there; this
// file pins that the control does not quietly say something the kernel would
// not do.
import { cleanup, fireEvent, render, screen, within } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import {
  FOLLOW_INSTALLATION_DEFAULT,
  type ModelCatalog, type ModelSelection,
} from '../../../../../core/domain/conversation.ts';
import { ModelPill } from './model-pill.tsx';

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

function trigger(name: RegExp): HTMLElement {
  return screen.getByRole('button', { name });
}

function openMenu(name: RegExp): HTMLElement {
  fireEvent.click(trigger(name));
  return screen.getByRole('menu');
}

describe('ModelPill', () => {
  /*
   * The trigger names the model and not the route to it: `gpt-5-codex`, never
   * `Default (gpt-5-codex)`. "Default" is a fact about how the answer was
   * arrived at, and the pill's job is the answer.
   */
  it('names the default it is actually following, and only the name', () => {
    render(
      <ModelPill catalog={catalog()} selection={FOLLOW_INSTALLATION_DEFAULT} onChange={vi.fn()} />,
    );
    expect(trigger(/^Model:/).textContent).toBe('gpt-5-codex');
  });

  /*
   * What the visible label dropped, the accessible one keeps: whether this
   * conversation has PINNED that model or is following whatever the
   * installation uses is a real difference, and a person reading the pill can
   * open the menu to see which row is ticked. A person hearing it cannot.
   */
  it('still says, to a screen reader, that it is following the default', () => {
    const { rerender } = render(
      <ModelPill catalog={catalog()} selection={FOLLOW_INSTALLATION_DEFAULT} onChange={vi.fn()} />,
    );
    expect(trigger(/^Model:/).getAttribute('aria-label'))
      .toBe("Model: gpt-5-codex (this installation's default)");

    rerender(
      <ModelPill
        catalog={catalog()}
        selection={{ model: 'gpt-5', reasoning_effort: null }}
        onChange={vi.fn()}
      />,
    );
    expect(trigger(/^Model:/).getAttribute('aria-label')).toBe('Model: GPT-5');
  });

  /*
   * And the menu keeps the word, because there it IS the distinction: its
   * first row is "follow whatever this installation uses", the rows under it
   * are that same model pinned by name, and without the word they would read
   * as the same choice twice.
   */
  it('keeps the word in the menu, where it is the choice being offered', () => {
    render(
      <ModelPill catalog={catalog()} selection={FOLLOW_INSTALLATION_DEFAULT} onChange={vi.fn()} />,
    );
    const menu = openMenu(/^Model:/);
    expect(within(menu).getByText('Default (gpt-5-codex)')).toBeTruthy();
  });

  /*
   * No chevron on either trigger (owner's call). Pinned because the control
   * then has no visual affordance at rest at all — if this ever comes back it
   * should be a decision, not a regression.
   */
  it('draws no chevron on the trigger', () => {
    render(
      <ModelPill catalog={catalog()} selection={FOLLOW_INSTALLATION_DEFAULT} onChange={vi.fn()} />,
    );
    expect(trigger(/^Model:/).querySelector('svg')).toBeNull();
  });

  /*
   * The complement, and the one that matters: with no workspace to resolve
   * config layers against, `GET /api/models` answers `default_source:
   * 'unknown'` and a `default.model` that would be a global-layer value. The
   * pill must not print it — naming a model it has not been told this
   * conversation follows is the same false statement in a smaller font.
   */
  it('will not name a default it was told it could not resolve', () => {
    render(
      <ModelPill
        catalog={catalog({ default_source: 'unknown', default: { model: 'gpt-5-global', reasoning_effort: null } })}
        selection={FOLLOW_INSTALLATION_DEFAULT}
        onChange={vi.fn()}
      />,
    );
    expect(trigger(/^Model:/).textContent).toBe('Default');
    expect(screen.queryByText(/gpt-5-global/)).toBeNull();
  });

  it('shows the chosen model by its display name', () => {
    render(
      <ModelPill
        catalog={catalog()}
        selection={{ model: 'gpt-5', reasoning_effort: null }}
        onChange={vi.fn()}
      />,
    );
    expect(trigger(/^Model:/).textContent).toBe('GPT-5');
  });

  /*
   * A slug we hold that the catalog does not list still names what this
   * conversation runs. Falling back to "Default" would say the opposite of the
   * truth, and falling back to nothing would leave the reader with no way to
   * find out.
   */
  it('falls back to the slug for a model the catalog does not list', () => {
    render(
      <ModelPill
        catalog={catalog()}
        selection={{ model: 'gpt-6-preview', reasoning_effort: null }}
        onChange={vi.fn()}
      />,
    );
    expect(trigger(/^Model:/).textContent).toBe('gpt-6-preview');
  });

  /*
   * The slug travels, the preset id does not — asserted in both directions so
   * a fixture whose two identifiers happen to be equal cannot let a read of
   * the wrong field pass. `id` exists only as a React key.
   */
  it('hands back the slug and never the preset id', () => {
    const onChange = vi.fn();
    render(
      <ModelPill catalog={catalog()} selection={FOLLOW_INSTALLATION_DEFAULT} onChange={onChange} />,
    );
    fireEvent.click(within(openMenu(/^Model:/)).getByRole('menuitem', { name: /GPT-5/ }));
    expect(onChange).toHaveBeenCalledWith({ model: 'gpt-5', reasoning_effort: null });
    expect(onChange.mock.calls[0]?.[0]).not.toMatchObject({ model: 'preset-fast' });
  });

  /*
   * Choosing a model clears the effort. An effort belongs to the model it was
   * chosen on: carrying `high` across to a model that does not offer it makes
   * a selection the server then has to correct, and the reader would see a
   * value they never picked appear a moment later.
   */
  it('drops the effort back to the default when the model changes', () => {
    const onChange = vi.fn();
    render(
      <ModelPill
        catalog={catalog()}
        selection={{ model: 'gpt-6-preview', reasoning_effort: 'high' }}
        onChange={onChange}
      />,
    );
    fireEvent.click(within(openMenu(/^Model:/)).getByRole('menuitem', { name: /GPT-5/ }));
    expect(onChange).toHaveBeenCalledWith({ model: 'gpt-5', reasoning_effort: null });
  });

  it('offers "Default" as a real choice and sends both nulls for it', () => {
    const onChange = vi.fn();
    render(
      <ModelPill
        catalog={catalog()}
        selection={{ model: 'gpt-5', reasoning_effort: 'high' }}
        onChange={onChange}
      />,
    );
    fireEvent.click(within(openMenu(/^Model:/)).getByRole('menuitem', { name: /^Default/ }));
    expect(onChange).toHaveBeenCalledWith({ model: null, reasoning_effort: null });
  });

  /*
   * The effort control is a property of the chosen model, so its presence is
   * decided per model and never assumed.
   */
  describe('the effort control', () => {
    /*
     * Following the installation default is still running a model, and this is
     * the case that used to have no effort control at all — which is every
     * conversation nobody has touched the pill on, i.e. the common one.
     *
     * The kernel takes it: `catalog_advice` (`routes/planner_model.rs`) stores
     * `{model: null, reasoning_effort: "high"}` unjudged, and says why in as
     * many words — with no model chosen there is no catalog entry to judge the
     * effort against.
     */
    it('offers the followed model’s efforts while no model is chosen', () => {
      render(
        <ModelPill
          catalog={catalog({ default: { model: 'gpt-5', reasoning_effort: 'low' } })}
          selection={FOLLOW_INSTALLATION_DEFAULT}
          onChange={vi.fn()}
        />,
      );
      const effort = trigger(/^Reasoning effort:/);
      /* The effort, not the route to it — same rule as the model trigger. */
      expect(effort.textContent).toBe('low');
      expect(effort.getAttribute('aria-label')).toBe('Reasoning effort: low (the default)');
      const menu = openMenu(/^Reasoning effort:/);
      expect(within(menu).getByText('Thinks longer.')).toBeTruthy();
    });

    it('sends the effort with a null model when the default is being followed', () => {
      const onChange = vi.fn();
      render(
        <ModelPill
          catalog={catalog({ default: { model: 'gpt-5', reasoning_effort: 'low' } })}
          selection={FOLLOW_INSTALLATION_DEFAULT}
          onChange={onChange}
        />,
      );
      const menu = openMenu(/^Reasoning effort:/);
      fireEvent.click(within(menu).getByText('high'));
      /* `model: null` survives — choosing an effort must not silently pin the
         conversation to whatever the default happens to be today. */
      expect(onChange).toHaveBeenCalledWith({ model: null, reasoning_effort: 'high' });
    });

    /*
     * And the limit of the above, which is the honest one: the control can
     * only list efforts it has an entry for. A default this catalog does not
     * carry gives it nothing to offer, and offering the *other* models' efforts
     * would be a list about a model this conversation is not running.
     */
    it('is absent when the followed model is not in the catalog', () => {
      render(
        <ModelPill catalog={catalog()} selection={FOLLOW_INSTALLATION_DEFAULT} onChange={vi.fn()} />,
      );
      expect(screen.queryByRole('button', { name: /^Reasoning effort:/ })).toBeNull();
    });

    it('is absent for a model that offers only one effort', () => {
      render(
        <ModelPill
          catalog={catalog({
            models: [model({ supported_reasoning_efforts: [{ reasoning_effort: 'medium', description: 'The only one.' }] })],
          })}
          selection={{ model: 'gpt-5', reasoning_effort: null }}
          onChange={vi.fn()}
        />,
      );
      expect(screen.queryByRole('button', { name: /^Reasoning effort:/ })).toBeNull();
    });

    it('offers the chosen model\'s efforts with codex\'s own descriptions', () => {
      render(
        <ModelPill
          catalog={catalog()}
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
        <ModelPill
          catalog={catalog()}
          selection={{ model: 'gpt-5', reasoning_effort: null }}
          onChange={onChange}
        />,
      );
      fireEvent.click(within(openMenu(/^Reasoning effort:/)).getByRole('menuitem', { name: /high/ }));
      expect(onChange).toHaveBeenCalledWith({ model: 'gpt-5', reasoning_effort: 'high' });
    });
  });

  /*
   * The two ways the catalog can be empty are different facts and must not
   * render the same. "codex is not running" is recoverable by starting codex;
   * "this account has nothing selectable" is not, and telling someone to start
   * a daemon that is already running wastes their time.
   */
  it('separates a daemon that could not be asked from an account with nothing to offer', () => {
    const { unmount } = render(
      <ModelPill
        catalog={catalog({ models: [], source: 'unavailable', default_source: 'config_toml' })}
        selection={FOLLOW_INSTALLATION_DEFAULT}
        onChange={vi.fn()}
      />,
    );
    const unreachable = trigger(/^Model:/);
    expect(unreachable.hasAttribute('disabled') || unreachable.getAttribute('aria-disabled') === 'true')
      .toBe(true);
    unmount();

    render(
      <ModelPill
        catalog={catalog({ models: [], source: 'live' })}
        selection={FOLLOW_INSTALLATION_DEFAULT}
        onChange={vi.fn()}
      />,
    );
    const live = trigger(/^Model:/);
    expect(live.hasAttribute('disabled') || live.getAttribute('aria-disabled') === 'true').toBe(false);
    expect(within(openMenu(/^Model:/)).getByText(/No models available on this account/)).toBeTruthy();
  });

  /*
   * The one sentence about switching models that is true every time. It is a
   * standing note rather than a warning fired at a particular switch: whether
   * a compaction happens depends on both models' context windows, and the
   * catalog carries none — a caption that asserted it each time would be false
   * far more often than not.
   */
  it('explains the compaction risk once, as a note rather than a per-switch warning', () => {
    render(
      <ModelPill catalog={catalog()} selection={FOLLOW_INSTALLATION_DEFAULT} onChange={vi.fn()} />,
    );
    const note = within(openMenu(/^Model:/)).getByRole('note');
    expect(note.textContent).toContain('smaller context window');
    expect(screen.queryByRole('menuitem', { name: /smaller context window/ })).toBeNull();
  });

  it('shows the default and stays quiet while the catalog is still loading', () => {
    render(
      <ModelPill catalog={null} selection={FOLLOW_INSTALLATION_DEFAULT} onChange={vi.fn()} />,
    );
    expect(trigger(/^Model:/).textContent).toBe('Default');
    expect(screen.queryByRole('button', { name: /^Reasoning effort:/ })).toBeNull();
  });

  it('is unavailable while its host says so', () => {
    const selection: ModelSelection = { model: 'gpt-5', reasoning_effort: 'high' };
    render(
      <ModelPill catalog={catalog()} selection={selection} onChange={vi.fn()} isDisabled />,
    );
    for (const name of [/^Model:/, /^Reasoning effort:/] as const) {
      const button = trigger(name);
      expect(button.hasAttribute('disabled') || button.getAttribute('aria-disabled') === 'true')
        .toBe(true);
    }
  });
});
