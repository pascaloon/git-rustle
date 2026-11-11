# Native UI conventions

- Make structure carry the explanation: group related actions, separate different
  tasks, and place controls beside the content they affect.
- Use the shared SVG icons for compact, familiar actions. Every icon-only action
  needs a specific tooltip, including its shortcut or consequence where useful.
  Do not use font glyphs as substitutes for action icons.
- Keep navigation names, field labels, values, live status, errors, and explicit
  confirmation consequences visible. Use a short verb label for primary actions
  and ambiguous Git operations. Do not turn unfamiliar operations into icon puzzles.
- Put instructional copy in the relevant control's tooltip or an adjacent info
  control. Avoid introductory paragraphs, repeated instructions, and marketing
  copy inside working views. Never hide a destructive consequence in a tooltip.
- Use consistent cards for Settings sections, a single accent treatment for the
  primary action, and spacing or separators for secondary action groups.
  Align Settings navigation, groups, and save controls on the same centered
  880-pixel content grid. Use compact rows with 148-pixel field labels and a fixed
  save bar outside the scrolling form. Put each icon-led section title inside its
  card in a tinted header with a separator. Theme and provider choices share the
  same segmented control. Accordion separators are explicit non-shrinking lines.
- Preserve disabled and selected states. Color supplements shape, labels, and
  grouping; it must not be the only indication of meaning.
- Check the minimum supported window size as well as the normal size. Keep
  contextual help attached to its field, avoid clipped actions, and let content
  scroll without displacing the view's action bar.

Follow [AGENTS.md](../AGENTS.md) for behavioral, performance, and isolated PNG
validation. Screenshot review is separate from action/paint assertions and does
not establish physical input behavior or display scanout.
