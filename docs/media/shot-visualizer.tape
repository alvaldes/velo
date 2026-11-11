# The visualizer over the whole window — the README's second image.
#
# Recorded by `docs/media/render.sh`. Same library and same playing track as
# `shot-library.tape`, so the two images read as one session.

Output shot-visualizer.gif
Set Shell "bash"
Set FontFamily "Menlo"
Set FontSize 20
Set Width 1400
Set Height 780
Set Padding 40
Set BorderRadius 8
Set MarginFill "#0f1620"
Set TypingSpeed 0ms
Set Theme { "name": "velo", "background": "#141c28", "foreground": "#f4f0e2", "cursor": "#f4baa3", "black": "#141c28", "red": "#f4baa3", "green": "#a6c8e2", "yellow": "#e8d9a0", "blue": "#a6c8e2", "magenta": "#d8b4e2", "cyan": "#9ad0c8", "white": "#f4f0e2", "brightBlack": "#9aa3ae", "brightRed": "#f4baa3", "brightGreen": "#a6c8e2", "brightYellow": "#e8d9a0", "brightBlue": "#a6c8e2", "brightMagenta": "#d8b4e2", "brightCyan": "#9ad0c8", "brightWhite": "#ffffff" }

Hide
Type "velo --library ~/Music"
Enter
Sleep 5s

# Sources → `Local library` → the tracks view → play → the full window.
Type "j"
Sleep 300ms
Enter
Sleep 2s
Type "3"
Sleep 700ms
Type "j"
Type "j"
Type "j"
Sleep 400ms
Enter
Sleep 6s
Type "V"
Sleep 4s
Show
Sleep 500ms
