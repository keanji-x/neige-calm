# Stroked icons

`ui/icon` keeps icon shape separate from the geometry of its control box. Every
icon uses the same 16×16 view box, 1.5px rounded stroke, visual centre, and
`currentColor`; the module CSS alone selects the 14px or 16px rendered size.

Text glyphs cannot provide that consistency because their visible ink is owned
by the font. In a real browser at 1440×900, with the same 28px box and 16px font
size, `‹`/`›` measured 5.3px wide, `↑` 8.0px, and `×`/`+` 9.3px. The widest
mark was therefore 1.75 times the narrowest even though their declared size was
identical. Shared stroked paths remove that font-metric variation.

The component is presentation-only and stateless. It may be imported by
`ui/**`, `features/**`, and `app/**`; callers own the control, accessible name,
state, and interaction while `Icon` owns only the named shape and glyph size.
