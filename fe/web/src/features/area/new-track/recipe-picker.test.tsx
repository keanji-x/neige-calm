// @vitest-environment jsdom
//
// The picker's recipe half (#1292 S4): the two labelled bands, and the fact
// that two id spaces sharing a string cannot be confused for one another.
//
// The wire-level assertion — that a create carries `recipe_id` XOR
// `template_id` — lives in `app/router/recipes-route.test.tsx`, because it is a
// claim about the request and this component never makes one. What is asserted
// here is the draft it hands its caller, which is the same fact one layer up.
import { act, cleanup, render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { DirectoryListing } from '../../../ui/directory-browser/public.tsx';
import type { TrackRecipe, TrackTemplate } from '../../../../../core/domain/track.ts';
import { NewTrackForm } from './public.tsx';

afterEach(cleanup);

const LISTING: DirectoryListing = { path: '/srv/app', parent: '/srv', entries: [] };
const TASK_LABEL = 'What this track should do';

const SMALL_CHANGE: TrackTemplate = {
  id: 'small-change',
  title: 'Small change',
  tasks: [{ key: 'inspect', goal: 'Read the requested change.' }],
};

/*
 * A recipe whose id is *the same string* as a built-in template's key.
 *
 * This is not contrived. Recipe ids are server-minted and templates' keys are
 * Rust constants; nothing in either system reserves the other's namespace, so
 * a collision is possible today and costs nothing to make impossible to
 * misread. The two rows below are what a bare-string selection could not tell
 * apart.
 */
const COLLIDING_RECIPE: TrackRecipe = {
  id: 'small-change', title: 'My small change', body: '## Mine\n',
  revision: 1, created_at: 1, updated_at: 1,
};

function renderForm(overrides: Partial<Parameters<typeof NewTrackForm>[0]> = {}) {
  const onSubmit = vi.fn();
  const props = {
    submitting: false,
    error: null,
    templates: [SMALL_CHANGE],
    templatesLoaded: true,
    initialTemplateId: null,
    initialCwd: null,
    onManageRecipes: vi.fn(),
    listDirectory: vi.fn(() => Promise.resolve(LISTING)),
    onSubmit,
    ...overrides,
  };
  return { props, onSubmit, ...render(<NewTrackForm {...props} />) };
}

const templateTrigger = () => screen.getByRole('button', { name: /^Template: / });

/* astryx focuses the first item on the frame after the menu opens, so the
   whole open has to be flushed inside `act` or every query after it races. */
async function openTemplates() {
  await userEvent.click(templateTrigger());
  await act(async () => {
    await new Promise((resolve) => { requestAnimationFrame(() => resolve(null)); });
  });
  return screen.getByRole('menu');
}

async function fillMessage() {
  await userEvent.click(screen.getByLabelText(TASK_LABEL));
  await userEvent.keyboard('Ship the thing');
}

const submitButton = () => screen.getByRole('button', { name: 'Create track' });

describe('the picker groups two kinds only when it has two kinds', () => {
  /*
   * Day one: the reader has no recipes. The menu must look exactly as it did
   * before recipes existed — no "My recipes" band over an empty stretch, and
   * no "you have no recipes yet" row taking up an option's worth of space in a
   * popover. That copy belongs on the manage screen, which is reached from the
   * one row this menu does gain.
   */
  it('renders no band headings when the reader has no recipes', async () => {
    renderForm({ recipes: [] });
    const menu = await openTemplates();
    expect(within(menu).queryByRole('group')).toBeNull();
    expect(within(menu).queryByText('My recipes')).toBeNull();
    expect(within(menu).queryByText('Built in')).toBeNull();
    expect(within(menu).getAllByRole('menuitem').map((item) => item.textContent))
      .toEqual(['No templateSelected', 'Small change', 'Manage recipes…']);
  });

  /*
   * A single band is not a grouping either: with recipes but no templates —
   * which is what a failed `GET /api/track-templates` looks like from here —
   * there is nothing to tell apart, so there is no heading.
   */
  it('renders no band headings when the reader has no built-ins', async () => {
    renderForm({ templates: [], recipes: [COLLIDING_RECIPE] });
    const menu = await openTemplates();
    expect(within(menu).queryByRole('group')).toBeNull();
  });

  it('names both bands once both kinds are present', async () => {
    renderForm({ recipes: [COLLIDING_RECIPE] });
    const menu = await openTemplates();
    /* Mine first, built-ins after: the reader's own work is what they came
       for, and the built-ins are the fallback. */
    expect(within(menu).getAllByRole('group').map((group) => group.getAttribute('aria-label')))
      .toEqual(['My recipes', 'Built in']);
    /* The visible heading is `aria-hidden` and the band carries the same
       string as its name, so the text is announced once on entry rather than
       met as an unowned node between menu items. */
    expect(within(menu).getByText('My recipes').getAttribute('aria-hidden')).toBe('true');
  });
});

describe('two id spaces, never crossed', () => {
  it('keeps an Area default in the template namespace when a recipe shares its id', async () => {
    const { onSubmit } = renderForm({
      recipes: [COLLIDING_RECIPE],
      initialTemplateId: 'small-change',
    });
    await fillMessage();
    expect(templateTrigger().getAttribute('aria-label')).toBe('Template: Small change');
    await userEvent.click(submitButton());
    expect(onSubmit).toHaveBeenCalledWith({
      message: 'Ship the thing', template_id: 'small-change',
    });
  });

  it('does not let a same-id recipe resolve an Area template while its roster is pending', async () => {
    const { onSubmit } = renderForm({
      templates: [],
      templatesLoaded: false,
      recipes: [COLLIDING_RECIPE],
      initialTemplateId: 'small-change',
    });
    await fillMessage();
    expect(templateTrigger().getAttribute('aria-label')).toBe('Template: small-change');
    expect((submitButton() as HTMLButtonElement).disabled).toBe(true);

    const menu = await openTemplates();
    await userEvent.click(within(menu).getByRole('menuitem', { name: /^No template/ }));
    expect((submitButton() as HTMLButtonElement).disabled).toBe(false);
    await userEvent.click(submitButton());
    expect(onSubmit).toHaveBeenCalledWith({ message: 'Ship the thing' });
  });

  /*
   * A recipe and a template with the same id string. Choosing the recipe must
   * produce `recipe_id`, and it must not produce `template_id` — which is the
   * thing a `templates.find((t) => t.id === selected)` over a bare string
   * would have got wrong, silently and in the direction that creates a track
   * from something the reader never picked.
   */
  it('creates from the recipe when a recipe and a template share an id', async () => {
    const { onSubmit } = renderForm({ recipes: [COLLIDING_RECIPE] });
    await fillMessage();
    await openTemplates();
    await userEvent.click(screen.getByRole('menuitem', { name: /^My small change/ }));
    // The chip names the recipe, not the identically-keyed template.
    expect(screen.getByRole('button', { name: 'Template: My small change' })).toBeTruthy();
    await userEvent.click(submitButton());
    expect(onSubmit).toHaveBeenCalledWith({ message: 'Ship the thing', recipe_id: 'small-change' });
  });

  /*
   * The stale-selection fallback, which is where the bare string did its
   * second kind of damage: a recipe deleted in another window resolved against
   * the *template* list, so the chip silently changed what it meant and the
   * create sent a `template_id` the reader had never chosen.
   *
   * Falling back to "no template" is the safe direction — it always submits —
   * and it is the only direction that does not invent a choice.
   */
  it('falls back to no template when the selected recipe disappears', async () => {
    const { onSubmit, rerender, props } = renderForm({ recipes: [COLLIDING_RECIPE] });
    await fillMessage();
    await openTemplates();
    await userEvent.click(screen.getByRole('menuitem', { name: /^My small change/ }));
    expect(screen.getByRole('button', { name: 'Template: My small change' })).toBeTruthy();

    // The recipe is deleted elsewhere and the list refetches without it. The
    // template of the same id is still there.
    rerender(<NewTrackForm {...props} recipes={[]} />);
    expect(screen.getByRole('button', { name: 'Template: No template' })).toBeTruthy();
    await userEvent.click(submitButton());
    expect(onSubmit).toHaveBeenCalledWith({ message: 'Ship the thing' });
  });
});

describe('the way to the manage screen', () => {
  /*
   * The only entry point to recipe authoring in the product, so it is present
   * whether or not the reader has any — and it is an action, not an
   * alternative: pressing it must not change what the track would start from.
   */
  it('is offered with no recipes, and selects nothing', async () => {
    const { props } = renderForm({ recipes: [] });
    await openTemplates();
    await userEvent.click(screen.getByRole('menuitem', { name: 'Manage recipes…' }));
    expect(props.onManageRecipes).toHaveBeenCalledTimes(1);
    expect(screen.getByRole('button', { name: 'Template: No template' })).toBeTruthy();
  });
});
