This is vt100 0.15.2 (https://github.com/doy/vt100-rust, MIT, see LICENSE), vendored
by omanote with two changes, both in `src/grid.rs`:

- `Grid::visible_rows` no longer panics when the scrollback offset is larger than
  the screen height.
- `Grid::scroll_up` keeps a line that leaves the top of the screen in the
  scrollback when a scroll region is set but starts at the top row. Upstream only
  keeps it with no region at all, so an agent that prints its chat above an input
  box (a region over the rows above it) lost everything that scrolled away. xterm,
  kitty and Alacritty keep those lines.

It is vendored rather than upgraded because vt100 0.16 needs unicode-width >= 0.2.1,
while ratatui 0.29 pins unicode-width to exactly 0.2.0. When ratatui moves on, drop
this folder and the `[patch.crates-io]` entry in the top-level Cargo.toml.
