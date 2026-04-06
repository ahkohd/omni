# Raycast Extension Research

> Generated 2026-04-05. See full research output below.

## Quick Reference

- **Scaffold**: Raycast → "Create Extension" → pick template
- **Dev mode**: `npm run dev` (hot reload in Raycast)
- **Command modes**: `view` (UI), `no-view` (instant action), `menu-bar` (status bar)
- **CLI execution**: `useExec` hook (view commands) or async function with `child_process` (no-view)
- **Preferences**: Defined in `package.json`, accessed via `getPreferenceValues<T>()`
- **Publishing**: Fork `raycast/extensions`, add extension dir, PR

## Key APIs

- `showHUD(message)` — brief overlay, closes Raycast (perfect for toggles)
- `showToast({ style, title, message })` — in-Raycast notification
- `useExec(cmd, args, opts)` — React hook for shell commands (from `@raycast/utils`)
- `getPreferenceValues<T>()` — read user preferences
- `List`, `Detail`, `Form`, `ActionPanel`, `Action` — UI components

## Command Modes

| Mode | Use Case | Returns |
|------|----------|---------|
| `no-view` | Quick actions (toggle) | async function or null component |
| `view` | UI (lists, details) | React component |
| `menu-bar` | Status indicator | MenuBarExtra component |
