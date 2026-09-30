# Template selection and preview

Selecting a starting point should explain its purpose and working method before creation. Keep the request composer primary; collect exact required parameters in a compact section attached to the selected template. Optional merge authorization stays visible as a consequence of the choice, with approval required by default.

## Boundaries

- Template authors own optional descriptive metadata (`description`, `instructions`) in TOML front matter, plus the actual report body and working method. Descriptions do not grant execution authority.
- Plugins continue to own their input schemas and tool behavior. Templates declare input controls and fixed converter mappings; platform-independent converters live in core. Input validation remains authoritative in the plugin contract.
- The server exposes a read-only template detail using the same roster as creation. The frontend requests the selected detail, displays metadata as text, and offers the complete source. No client-maintained template descriptions.
- The frontend owns layout, input assistance, loading/error states and removal of the selection. Kernel task scheduling, approval and merge mechanisms remain unchanged.

## Acceptance

The selected template or recipe appears beneath the composer; purpose, working method and full source are discoverable. Selection changes cannot show stale details. Loading failures offer retry; bound templates wait for a matching valid form declaration before creation. Issue URL is required only for a bound issue template; merge approval remains the default and the authorized choice is preserved in the submitted input. Unknown template ids return 404; detail reads change no state. Desktop, narrow-screen and keyboard interactions remain usable. Preview uses a separate server and state directory.

## Template-owned input forms

The template body may contain one versioned `neige:input-form` JSON comment. It declares groups, field labels, defaults, text or toggle controls, and explicit mappings from supported URL formats to plugin input keys. The creation feature renders this data for any template id; no issue-development branch or merge-policy constants live in the form. The existing plugin input schema remains the authoritative wire constraint. Missing or invalid form definitions on a bound template block creation with a readable notice. Unbound templates collect no plugin input.

The URL converter is a platform-independent, fixed parser; templates cannot execute scripts. Form values are retained by template id in the route's in-memory draft. A selected template's metadata must match its id before any field is rendered or submitted. Source preview remains text-only.

Issue information and merge authorization have separate labelled groups. Disclosure controls use the existing outline chevron with native keyboard interaction. The template picker supplies No template; duplicate removal buttons and default-status labels are removed.

For one-field groups, show the field label once and retain the group name as an accessible label. Author one useful field description instead of repeating group introductions or required-state captions. Fields use ordinary controls and declared defaults; no additional card surfaces or sample values masquerading as defaults.

Each target input key has one semantic writer. The canonical URL field and an explicit canonical-URL identity mapping may share that writer; different converter outputs cannot overwrite one target, even when they originate in the same field.
