---
name: KeyCompute Admin Console
source: awesome-design-md/design-md/linear.app/DESIGN.md
reference: Linear-inspired authenticated console
---

# Admin Console Design Direction

This project uses the Linear DESIGN.md reference for authenticated backend pages.
The goal is a quiet, technical console: dense enough for operators, low visual
noise, and focused on status, tables, and repeated administrative actions.

## Visual Tokens

- Canvas: near-black `#010102` in dark mode.
- Surfaces: charcoal panels from `#0f1011` to `#18191a`.
- Accent: lavender-blue `#5e6ad2`, used only for active navigation, focus
  states, primary buttons, and selected status details.
- Text: high-contrast gray `#f7f8f8`, muted gray `#8a8f98`, tertiary gray
  `#62666d`.
- Borders: one-pixel hairlines, mainly `#23252a` and `#34343a`.
- Radius: compact product UI radius, mostly `6px` to `12px`.

## Layout Rules

- Authenticated pages should feel like a software control plane, not a
  marketing page.
- Use full-width work surfaces with constrained inner content.
- Prefer thin borders and restrained hover states over heavy shadows.
- Cards should frame actual data modules only: metrics, tables, forms, and
  status panels.
- Avoid decorative gradients, large glows, and oversized hero treatments inside
  the console.

## Component Rules

- Sidebar stays dark and compact, with active state carried by accent color and
  a narrow indicator.
- Header is a low-height command bar with hairline separation.
- Tables use uppercase muted headers, compact row spacing, and subtle row hover.
- Buttons use 8px radius, primary accent fill, and quiet secondary surfaces.
- Status badges use tinted backgrounds with semantic color, not saturated fills.

## Typography

- Use Inter/SF system stack.
- Use 13px to 15px for dense operational text.
- Use 16px to 20px for page and panel titles.
- Avoid viewport-scaled typography in the authenticated app shell.
