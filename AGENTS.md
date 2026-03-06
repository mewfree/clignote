# AGENTS.md — clignote

Instructions for AI agents working on this codebase.

## project overview

**clignote** is a terminal-first org-mode editor with evil-mode (vim) keybindings.
Binary name: `cln`. Written in Rust.

Potential future targets: native macOS desktop app, mobile (Android/iOS).

## repository layout

```
clignote/
  readme.org          — user-facing readme
  AGENTS.md           — this file
  project.org         — task tracker
  Cargo.toml          — workspace root
  crates/
    cln/              — binary entry point (thin shell, delegates to clignote-tui)
    clignote-core/    — org-mode parser + document model (no I/O, no UI)
    clignote-tui/     — terminal UI using ratatui
```

Future crates: `clignote-desktop`, `clignote-mobile`.

## conventions

- Rust edition 2021, stable toolchain
- Format with `cargo fmt` before committing
- Lint with `cargo clippy -- -D warnings`
- Tests live in the same file as the code (`#[cfg(test)]`) for unit tests,
  and in `tests/` for integration tests
- No `unwrap()` in library code — use proper error propagation with `thiserror`
- Use `anyhow` in binary / application code for error handling

## org-mode parsing

- The parser lives exclusively in `clignote-core`
- It should be a lossless parser: round-tripping a file must produce identical bytes
- Represent the document as a tree: `Document > Section > Block > Inline`
- Do not use external org-mode parsing crates unless evaluating for correctness;
  build our own to control fidelity

## evil-mode / vim keybindings

- Modal editing: Normal, Insert, Visual modes (Command mode `:` later)
- Key handling lives in `clignote-tui`
- Implement motions as composable commands (operator + motion pattern)
- Follow Neovim semantics where there is ambiguity

## testing

```sh
cargo test              # run all tests
cargo test -p clignote-core   # core parser tests only
```

## task tracking

Tasks are tracked in `project.org` using the `** TODO XXX` format.
Update task status there as work progresses (`TODO` → `DONE`).

## things to avoid

- Do not add GUI dependencies to `clignote-core` — it must stay headless
- Do not parse org-mode with regexes alone — build a proper recursive descent parser
- Do not break the lossless round-trip property of the parser
