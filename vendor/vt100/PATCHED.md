This is vt100 0.15.2 (https://github.com/doy/vt100-rust, MIT, see LICENSE), vendored
by omanote with one change, in `src/grid.rs`: `Grid::visible_rows` no longer panics
when the scrollback offset is larger than the screen height.

It is vendored rather than upgraded because vt100 0.16 needs unicode-width >= 0.2.1,
while ratatui 0.29 pins unicode-width to exactly 0.2.0. When ratatui moves on, drop
this folder and the `[patch.crates-io]` entry in the top-level Cargo.toml.
