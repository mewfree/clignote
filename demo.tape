Output demo.gif

Set Shell "zsh"
Set FontSize 14
Set Width 1200
Set Height 700
Set Theme "Dracula"
Set TypingSpeed 80ms

# --- hidden setup ---
Hide

Type "bash /Users/damien/dev/clignote/demo-setup.sh && cd /tmp/clignote-demo"
Enter
Sleep 800ms

Show
Type "clear"
Enter
Sleep 300ms

# Open the file — syntax-highlighted org-mode renders immediately
Type "cln notes.org"
Enter
Sleep 1500ms

# Navigate down through the headings with vim keys
Type "j"
Sleep 220ms
Type "j"
Sleep 220ms
Type "j"
Sleep 220ms
Type "j"
Sleep 220ms
Type "j"
Sleep 220ms
Type "j"
Sleep 220ms
Type "j"
Sleep 1000ms

# Search — incremental highlights update as each char is typed
Type "/TODO"
Sleep 1500ms

# Commit search (→ NORMAL mode, cursor at first match row 7)
Enter
Sleep 400ms

# Jump to next match with n (now in NORMAL mode)
Type "n"
Sleep 800ms

# One more — lands on "** TODO [#A] release 0.1.0" (row 14)
Type "n"
Sleep 800ms

# Clear search highlights, cursor stays at row 14
Escape
Sleep 500ms

# Navigate down to the unchecked checkboxes (j×4 → row 18)
Type "j"
Sleep 220ms
Type "j"
Sleep 220ms
Type "j"
Sleep 220ms
Type "j"
Sleep 800ms

# Toggle "- [ ] org-mode rendering" → [X]
Enter
Sleep 900ms

# Move to next unchecked item and toggle it too
Type "j"
Sleep 300ms
Enter
Sleep 900ms

# Open a new line below and add a task in INSERT mode
Type "o"
Sleep 400ms
Type "  - [ ] write release notes"
Sleep 600ms
Escape
Sleep 600ms

# Save and quit
Type ":wq"
Enter
Sleep 800ms
