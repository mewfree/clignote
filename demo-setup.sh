#!/usr/bin/env bash
set -e

rm -rf /tmp/clignote-demo
mkdir -p /tmp/clignote-demo

cat > /tmp/clignote-demo/notes.org << 'EOF'
#+title: project notes

* clignote                                                          :project:

  A terminal-first org-mode editor with evil-mode (vim) keybindings.
  Written in Rust.

** TODO write parser tests                                          :dev:
** DONE implement lexer                                             :dev:
** TODO add syntax highlighting                                     :dev:
** DONE set up crate structure                                      :dev:

* tasks

** TODO [#A] release 0.1.0                                         :milestone:

  - [X] basic navigation  (hjkl, w, b, gg, G)
  - [X] insert / normal mode
  - [ ] org-mode rendering
  - [ ] file picker

** TODO [#B] write documentation

* links

  - [[https://orgmode.org][org-mode documentation]]
  - [[https://ratatui.rs][ratatui]] — TUI framework
  - +deprecated: tui-rs+
EOF
