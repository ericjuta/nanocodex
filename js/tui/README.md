# nanocodex-tui

Framework-independent transcript state and event reduction for Nanocodex TUI
renderers. It has no React, DOM, transport, or styling dependency.

It also owns the typed TUI controller protocol (`TuiCommand`, `TuiMessage`, and
`TuiTarget`) shared by a renderer and its agent Worker.

Most applications should use `nanocodex-tui-react`. Use this package directly
when implementing a renderer for another UI framework.
