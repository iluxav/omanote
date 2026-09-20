 <img src="assets/icon-256.png" width="96" align="right" alt="omanote icon">

# omanote

A small markdown note editor for the terminal. Think *nano for markdown*: open it and type. There are no modes and nothing to configure, and it is a single binary.

Markdown renders as you write. Headings, **bold**, links, checkboxes, tables and images show formatted, and the raw syntax only appears on the line you are editing.

![omanote editing a note: a heading, an image, a task list and a table, all rendered in the terminal](assets/screenshot.png)

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/iluxav/omanote/main/install.sh | sh
```

This downloads the latest release for your machine (Linux or macOS, x86_64 or ARM), verifies its checksum and puts `omanote` in `~/.local/bin`.

| Option | |
| --- | --- |
| `OMANOTE_VERSION=v0.1.0` | install a specific release |
| `OMANOTE_INSTALL_DIR=/usr/local/bin` | install somewhere else |

Uninstall (your notes are kept):

```sh
curl -fsSL https://raw.githubusercontent.com/iluxav/omanote/main/install.sh | sh -s -- --uninstall
```

### From source

Needs a [Rust toolchain](https://rustup.rs).

```sh
git clone https://github.com/iluxav/omanote && cd omanote
make install      # builds and copies to ~/.local/bin
make uninstall
```

## Use

```sh
omanote                 # a new, empty note
omanote readme          # find a note by name and open it
omanote notes/idea.md   # open that file, or start it if it does not exist
omanote --demo          # a note that shows everything off
```

`omanote <name>` looks in the current folder and in every vault. Case and the `.md` do not matter, so `omanote readme` opens `README.md` and `omanote my ideas` opens `My Ideas.md`. One match opens straight away; with several you get the list (`Tab` to move, `Enter` to open); with none you get a new note, and `Ctrl+S` offers to save it under that name.

Write first, name it later: on a new note `Ctrl+S` asks where to keep it (a vault, or the current folder), with the file name prefilled from your first line (`# Trip plan` → `trip-plan.md`). From then on notes save themselves as you type. A note that has no file yet is never dropped silently: quitting, `Ctrl+N` or opening another note asks where to save it first (or `Ctrl+D` to discard it).

| Key | |
| --- | --- |
| `Ctrl+N` | start a new note |
| `Ctrl+P` | find a note by fuzzy search, or create one |
| `Ctrl+S` / `Ctrl+Q` | save / quit |
| `F2` | move, rename or copy the note: another vault, another folder, another name |
| `Ctrl+G` | an AI agent of your choice in a pane beside the note |
| `Ctrl+Z` / `Ctrl+Y` | undo / redo |
| `Ctrl+C` / `Ctrl+X` / `Ctrl+V` | copy / cut / paste |
| `Shift+arrows`, `Ctrl+A` | select |
| `Ctrl+arrows` | move by word |
| `Ctrl+B` / `Ctrl+I` | bold / italic |
| `Ctrl+T` | toggle a task checkbox |
| `Enter` | continues lists, numbering and quotes |

The mouse works too: click, drag to select, double-click a word, scroll, click a checkbox.

### An assistant beside the note

`Ctrl+G` opens an AI agent in a pane on the right, inside omanote's own window: a real terminal running the agent's own CLI.

- **No agent is built in.** omanote looks for the agent CLIs you have installed (Claude Code, Codex, Gemini, opencode, Qwen Code, Aider, Cursor Agent, GitHub Copilot, Crush, Amp, Goose) and asks which one: arrows and `Enter`, or its number. It starts on the one you used last. With a single agent installed there is nothing to ask.
- It starts in the note's vault, so it can see your other notes, and it is told which note you are in, the line your cursor is on, and any text you have selected. Claude Code takes that silently, as an addition to its system prompt, so nothing is sent until you type; Codex, Gemini, opencode and Qwen take it as their opening message; the others are simply started in the right folder.
- `Ctrl+G` again moves the keyboard between the note and the agent; a click does the same. Quit the agent and the pane closes. If it quits the moment it starts, the pane stays up so you can read why.
- The note is saved before the agent opens, saves as you type, and reloads by itself when the agent edits the file, so you can watch changes land.
- In the pane, drag with the mouse to select text: it is copied when you let go, ready to paste into the note with `Ctrl+V`. The wheel scrolls back through the conversation; any key returns to the present.
- The pane's title names the agent and the note it was given.
- In a window too narrow for two columns, whichever side has the keyboard gets the whole window.

Add your own agents, change how a known one is started, or skip the menu, in the settings (`omanote --config`):

```toml
agent.Work bot = "workbot --chat {context}"
agent.claude   = "claude --model opus --append-system-prompt {context}"
assistant      = "codex"          # always this one: a name, or a full command
```

`{context}` is the description above, `{file}` the note and `{dir}` the folder the agent starts in. The pane is a small terminal emulator: colours, full-screen programs and paste work. The mouse belongs to omanote (select, scroll), so the agent itself never sees it, and terminal-specific extras are not passed through.

### Tables

Type a header row such as `| Item | Price |` and press `Enter`; the rest of the table is created for you.

| Key | |
| --- | --- |
| `Tab` / `Shift+Tab` | next / previous cell |
| `Enter` | down one row (adds a row at the bottom) |
| `Shift+Enter` | insert a row below |
| `Alt+Shift+→` / `Alt+Shift+←` | insert a column |

A table wider than the window shrinks to fit: its columns narrow and long cells wrap onto several lines.

### Images

A line that is only an image shows the picture underneath it:

```markdown
![a diagram](pics/diagram.png)
![from the web](https://example.com/photo.jpg)
```

Paste a screenshot with `Ctrl+V` and it is embedded in the note itself, so the file stays self-contained.

Real images need a terminal with the Kitty graphics protocol (Ghostty, Kitty). Other terminals get a low-resolution preview made of coloured blocks. Remote images are downloaded in the background and cached; set `OMANOTE_REMOTE_IMAGES=off` to never fetch them.

## On Omarchy

```sh
omanote --omarchy
```

adds omanote to the app launcher (through `omarchy-tui-install`) and a Notes section to the Omarchy menu. It prints what it does, changes nothing that is already there, and is safe to run twice. On other desktops it writes a plain `.desktop` entry.

For a notes icon in the bar and a quick-capture box, there is a plugin:

```sh
omarchy plugin add https://github.com/iluxav/omarchy-omanote --enable
```

`omanote --capture "call the dentist"` is what quick capture runs: it appends a line to `inbox.md` in your default vault, under a heading for the day, without opening the editor.

## Settings

`omanote --config` opens the settings file (`~/.omanote/config.toml`) in omanote itself, creating it with comments the first time:

```toml
width = 84          # widest the text column gets; 0 = the whole window
align = "center"    # where the column sits in a wide window: "left", "center", "right"
margin = 2          # blank columns kept at the window's edges
```

Saving applies the settings straight away, with no restart. A line that makes no sense is reported and nothing changes. The footer and panels take their colours from your terminal's own background and foreground, so they follow its theme; `OMANOTE_COLORS=plain` turns that off. `Ctrl+L` repaints the screen if a terminal ever garbles it.

### Files that are not markdown

Markdown rendering is for new notes and `.md` / `.markdown` files. Anything else, such as the settings file, a script or a log, opens as plain text: nothing is hidden or restyled, tables and lists are not touched, and `Enter` just keeps the indentation. Comment lines in config files (`.toml`, `.conf`, `.yaml`, `.sh`, …) are shown quieter.

## Vaults

A vault is a folder of notes that `Ctrl+P` searches. New notes go in the default vault, `~/.omanote/docs`. Add more:

```sh
omanote --vl ~/work/notes          # a folder
omanote --vlgh owner/repo          # clone a GitHub repo and add it
omanote --vls                      # list vaults
omanote --vlrm notes               # forget a vault (files are kept)
```

Vaults are recorded in `~/.omanote/origins.toml`. GitHub vaults are cloned into `~/.omanote/vaults`.

### GitHub vaults sync themselves

Notes in a `--vlgh` vault are kept in step with the repo, in the background:

- Opening a note pulls. You see the note straight away; if the pull changed it, it reloads.
- Saving commits and pushes, once the note has been quiet for 30 seconds, and when you switch notes or quit. Quitting does not wait for the network: the push finishes on its own.
- The status line shows `⇅ github`, `⇅ to sync` or `⇅ syncing…`.

To sync by hand, and see what happened, run `omanote --sync`: it goes through every GitHub vault, commits anything uncommitted, pulls, then pushes, and exits non-zero if a vault failed.

If the same note was changed in two places, your version is committed locally, nothing is overwritten, and omanote tells you to resolve it with git. Offline, commits wait and go out the next time you open or save a note in that vault. Set `OMANOTE_SYNC=off` to turn all of this off. Folders added with `--vl` are never committed to, even if they are git repositories.

## Development

```sh
make test       # run the tests
make release    # tag the version in Cargo.toml and push the tag
```

Pushing a `v*` tag makes GitHub Actions build the binaries and publish the release that `install.sh` downloads.

## License

[MIT](LICENSE)
