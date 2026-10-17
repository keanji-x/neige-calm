# Styles owner

`theme.config.json` is the editable typography, spacing and theme registry. The
build-time Zod schema validates paired steps, shared profiles, semantic bindings
and their references. Keep a distinct semantic role when its purpose differs;
roles may bind the same profile without copying its recipe.

Run `npm run theme:generate` after configuration or generator changes. It writes
`tokens.css`, host constants in `theme-values.ts`, closed typography/spacing/
navigation types and machine inventory in `typography.generated.ts`, and the
UI-owned inherited-font scope. `npm run theme:check` compares every artifact.

The generator records each emission's name, value, public category and font-family
dependency together. Duplicate names fail before any output is written. CSS,
public type families, inventory and font scopes consume that same record; none
classifies an open semantic-role name by its suffix. Stable colors, geometry,
motion and stacking types remain in `public.ts`.

Independent contracts pin the Radix paired scale, deployed font families,
unknown-token rejection, actual rendered typography, contrast floors, theme
parity and terminal/host values. Machine inventory is derived; these independent
oracles must not be replaced with generated-output equality alone.

`entry.css` declares the cascade and is the sole global entry. Third-party CSS
enters through `vendor.css`; the bridge maps only published vendor hooks. Global
selectors and intentional unlayered exceptions remain explicitly registered.
UI/features CSS Modules retain their owning layers. This leaf has no runtime
component or domain policy.

The explicit `ui/typography-scope` class regenerates only inherited-family
emissions. Mobile font assets select the existing self-hosted stacks on that
scope. CodeMirror geometry, terminal font/grid and host RGB retain their existing
contracts. Semantic colors, shadows, radii and component interactions are outside
the typography/spacing migration. See the repository design-system document for
the scale and the limited editor/compact-number exceptions.
