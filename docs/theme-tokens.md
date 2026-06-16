# Theme Token Contract

Lantor themeable CSS must use semantic tokens for surfaces, ink, borders, state, and status colors. Component and mobile rules should not hard-code light or dark colors directly.

## Token Groups

Use these groups before adding component-specific tokens:

- `--surface-*` for backgrounds and elevated planes.
- `--ink-*` for text and icon foregrounds.
- `--border-*` for dividing lines and component outlines.
- `--state-*` for hover, selected, active, focus, and transient states.
- `--status-*` for danger, warning, and success UI.
- `--badge-*` for compact count and status badges.

## Surface Roles

- `--surface-app`: app background.
- `--surface-sidebar`: sidebar and mobile drawer background.
- `--surface-panel`: default content panel.
- `--surface-panel-strong`: stronger panel used for cards and nav shells.
- `--surface-elevated`: raised rows or nested panels.
- `--surface-recessed`: inset surfaces inside a modal or panel (selection lists, embedded pickers, "wells" that should read as deeper than their container in both light and dark).
- `--surface-floating`: popovers, menus, modals, and floating nav.
- `--surface-sticky`: sticky headers and toolbars.
- `--surface-input`: text fields and editable surfaces.
- `--surface-composer`: message composer surfaces.
- `--surface-control-*`: buttons, segmented controls, tabs, and option rows.
- `--surface-code`: pre/code blocks.

## Migration Rules

1. Base component rules should use semantic tokens, not raw `#...`, `rgb(...)`, or `rgba(...)` values.
2. `@media` blocks are not exempt. Mobile CSS must use the same tokens as desktop CSS.
3. Prefer replacing literals with tokens in the base rule over adding a `:root[data-theme="dark"] .component` override.
4. Component-specific tokens are allowed only when a semantic token group cannot describe the role.
5. New status colors require all three roles: text, soft background, and border.

## Runtime Parity

Theme and UI preferences must share one configuration model across web and desktop shells. The Tauri desktop app, browser preview, and mobile browser view should read and write the same frontend settings path and render through the same React/CSS token code path.

Do not add a desktop-only or web-only preference path for theme, density, display mode, or message rendering behavior. If a platform needs a native bridge or persistence adapter, keep it behind the same setting key and the same UI state shape so the rendered result cannot drift by shell.

## Follow-Up Gates

Before the raw-color migration is considered complete:

- Style lint must reject new raw color literals outside token declarations.
- Dark and light screenshots must cover desktop and mobile breakpoints.
- Mobile coverage must include sidebar, bottom nav, thread, search, activity feed, modal, and composer.
- Settings coverage must verify that web and desktop shells use the same preference keys and render the same state.
