# Icon system

`ui/icon` is the single semantic icon registry. Functional shapes come from the
pinned `lucide-react` 0.468.0 package; Tools uses Wrench, terminal resources use
SquareTerminal, agents use Bot, and disclosures use ChevronRight. Running and
waiting use LoaderCircle and Clock3 respectively. Status circles remain distinct
from plain action controls.

All graphics use Lucide's 24-unit SVG geometry, 2-unit rounded strokes and the
same renderer. `--glyph-sm` supplies 14px auxiliary icons and `--glyph` supplies
16px primary icons. There are no optical-inset groups, runtime geometry rewrites,
brand-specific stroke overrides or DOM decoration. Controls own their hit areas.

The approved lightweight Claude/Codex artwork is registered as immutable path
data through the same Lucide renderer. Its coordinates are already normalized
to the shared grid. The [Codex source](https://github.com/lobehub/lobe-icons)
is credited in CODEX-LICENSE.txt; its browser contract pins the complete closed outline.

The primitive contains no application policy. Features own kind-to-icon mappings
and accessible names; core view derivation owns classifications and authoritative
attention metadata. Unknown kinds preserve their text and actions.
