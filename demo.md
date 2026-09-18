# Welcome to omanote

This is the **proof of concept**. Move the cursor onto any line and its raw markdown appears; move away and it *renders* again. Just type — there are no modes.

## Try these

- Arrow keys move, `Shift+arrows` select, `Ctrl+arrows` jump by word
- `Ctrl+Z` undo, `Ctrl+Y` redo, `Ctrl+C` / `Ctrl+X` / `Ctrl+V` clipboard
- Select some words and press `Ctrl+B` for **bold** or `Ctrl+I` for *italic*
- Press `Enter` at the end of this line and the list continues by itself
- The mouse works too: click, drag to select, double-click a word, scroll

## Tasks

- [ ] Click this checkbox, or press `Ctrl+T` on the line
- [x] Finished tasks get ~~struck through~~
- [ ] Links hide their URL: [Omarchy](https://omarchy.org) and [[wiki links|like this]]

## Other blocks

> Quotes get a bar down the side, and long ones wrap nicely underneath it so the text stays aligned with itself.

1. Numbered lists
2. Continue numbering when you press Enter

| Key          | What it does            | Where |
| ------------ | :---------------------: | ----: |
| `Tab`        | jump to the *next* cell | table |
| `Shift+Tab`  | back one cell           | table |
| `Enter`      | down one row            | table |
| `Shift+Enter` | insert a **new row**   | table |
| `Alt+Shift+→` | insert a new column    | table |

```rust
fn main() {
    println!("code blocks keep their markdown literal: **not bold**");
}
```

---

Tags like #idea and ==highlights== work, and a long paragraph soft-wraps at word boundaries inside a centred column, the way a notes app should, instead of running off the edge of the terminal.
