# A radio channel: the live row, then the songs it lists — the README's third
# image.
#
# Recorded by `docs/media/render.sh`. This one asks the directory over the
# network, so it needs a connection and it is the image that ages fastest.

Output shot-radio.gif
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

# `Local library` first, so the radio is opened over a library view: that is
# the screen whose footer the radio takes over, and the one this image shows.
Type "j"
Sleep 300ms
Enter
Sleep 2s

# `R`, then `cliamp radio`, then its Lofi channel: a live row, then the songs.
Type "R"
Sleep 4s
Enter
Sleep 6s
Enter
Sleep 14s

# The live row plays the channel itself: the now-playing block names it and the
# spectrum comes alive, which is what a radio image is for.
Enter
Sleep 16s
Show
Sleep 500ms
