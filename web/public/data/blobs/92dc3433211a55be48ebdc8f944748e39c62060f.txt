# Product ideas

## Local UI feedback annotations

Add a development-only annotation layer for leaving feedback while clicking
through the site locally.

The smallest useful flow would be:

1. Enter comment mode from a small `Feedback` control or keyboard shortcut.
2. Hover to highlight a DOM element, then click it.
3. Write a short comment in a compact side panel.
4. Save the comment with its route, a stable CSS selector, accessible label,
   nearby text, viewport size, and timestamp.
5. Persist the notes to an ignored JSON file in the project so Codex can read
   them directly on the next iteration.

Comments could render as numbered pins and support locate, copy-all, and delete.
Keep the feature local-only; it does not need accounts, hosted persistence, or a
production feedback service.
