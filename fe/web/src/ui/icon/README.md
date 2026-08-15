# Stroked icons

`ui/icon` keeps icon shape separate from the geometry of its control box. Every
icon shares one line drawing and one 0.85 optical-inset ratio in a 16×16 view
box. The source `stroke-width` is 1.5; after the inset and viewBox-to-viewport
mapping it renders as 1.275px at md (16px) and 1.116px at sm (14px).
`currentColor` supplies the colour and module CSS selects the rendered size.

Text glyphs cannot provide that consistency because their visible ink is owned
by the font. In a real browser at 1440×900, with the same 28px box and 16px font
size, `‹`/`›` measured 5.3px wide, `↑` 8.0px, and `×`/`+` 9.3px. The widest
mark was therefore 1.75 times the narrowest even though their declared size was
identical. Shared stroked paths remove that font-metric variation.

The component is presentation-only and stateless. It may be imported by
`ui/**`, `features/**`, and `app/**`; callers own the control, accessible name,
state, and interaction while `Icon` owns only the named shape and glyph size.
