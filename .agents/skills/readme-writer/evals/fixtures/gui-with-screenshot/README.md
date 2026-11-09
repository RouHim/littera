# Shotty

Shotty is a desktop screenshot annotator built with Rust and iced. It supports
rectangles, arrows, text labels, blur and export. The renderer uses tiny-skia
and the application stores annotations in a SQLite database. Build with
`cargo build --release`. The configuration lives in
`~/.config/shotty/config.toml`.

## Architecture

The capture thread sends RGBA frames to the UI thread. The editor model is a
pure state machine. Export uses PNG and WebP. The database schema has three
tables: documents, annotations and assets.

## Build dependencies

- Rust 1.98
- cmake
- libxkbcommon
- sqlite

## Release

Semantic release tags the repository. The AUR package is generated from the
release tarball.
