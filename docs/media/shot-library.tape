# The library's tracks view while a track plays — the README's first image.
#
# Recorded by `docs/media/render.sh`: vhs writes a GIF frame by frame and
# ffmpeg keeps the last one, because vhs 0.12 cannot write a still PNG.
#
# The folder is named on the command line: `velo` alone opens whatever this
# machine saved in `config.toml`, which is not what a screenshot should show.

Output shot-library.gif
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

# Sources → `Local library` → the tracks view → play the fourth row, so the
# now-playing block, the spectrum and the footer all have something to say.
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
Sleep 7s
Show
Sleep 500ms
