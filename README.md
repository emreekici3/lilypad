# lilypad

A floating window manager for macOS with smooth animations.

No tiling. Windows stay where you put them. SIP stays on.

## Install

```sh
git clone https://github.com/emreekici3/lilypad.git
cd lilypad
cargo install --path . --root ~
mkdir -p ~/.config/lilypad
cp examples/lilyrc ~/.config/lilypad/lilyrc
lily install
lily start
```

Make sure `~/bin` is on your `PATH`, then allow `lily` under Privacy & Security > Accessibility.

## Commands

```
lily move left
lily resize right
lily resize left 20
lily focus up
lily snap down
lily place center 70%
lily place left 60%
lily place max
lily close
```

Directions are `left`, `right`, `up`, `down`. Places are `center`, `left`, `right`, `top`, `bottom`, `top-left`, `top-right`, `bottom-left`, `bottom-right`, `max`.

## Spaces

```
lily space 2
lily space next
lily space recent
lily send 3
```

`space` shows a Space of the display, numbered from 1 as in Mission Control; `next` and `prev` go round, `recent` goes back to the last one. `send` moves the focused window to a Space and takes you there with it, the window still in focus.

Both happen at once, with SIP on. If macOS will not switch at once, lilypad presses the *Switch to Desktop N* shortcut instead, so turn those on in *System Settings → Keyboard → Keyboard Shortcuts → Mission Control*. If macOS will not move a window, it stays where it is and you stay with it: lilypad never takes hold of a window with the pointer. `lily doctor` says which of these work on your Mac.

Name Spaces, and send apps to them as their windows open:

```
lily label 2 web
lily space web
lily rule space web Safari --follow
```

With `--follow` you go along with the new window; without it, it opens there and you stay. Windows open before the rule stay where they are.

## Scratchpads

A scratchpad is an app at hand on every Space:

```
lily scratchpad term kitty
lily bind 'alt - s : lily scratchpad toggle term'
```

The key brings its window to the Space you are on, floating in the middle (or at the grid cell given after the app, e.g. `scratchpad term kitty 6:6:1:1:4:4`) and in focus, starting the app if it is not running. Pressed again it hides the app and the focus goes back. Leaving the Space hides it too. Choose an app you keep for this: hiding hides all of its windows.

## Grid

```
lily grid 2:2:1:0:1:2
lily rule grid 1:2:0:0:1:1 Zed
```

`grid rows:cols:x:y:w:h` puts it at a cell of a grid over its display, `w` x `h` cells large from column `x`, row `y` (from 0): `2:2:1:0:1:2` is the right half. `rule grid` does that to an app's new windows.

Open an app's new windows at a grid cell (here the left half):

```
lily rule grid 1:2:0:0:1:1 Zed
```

## Shortcuts

```
lily bind 'shift + alt - left : lily move left'
```

Hold two arrows to go diagonal. Two `place` shortcuts to sides pressed together (say right and down) put the window in that corner; one alone goes to its side a tenth of a second later, or as soon as its key comes up. Hold `alt` and drag to move a window, or right-drag to resize it.

## Config

`~/.config/lilypad/lilyrc` is a shell script. Start from [`examples/lilyrc`](examples/lilyrc).

```
lily config gap 12
lily config step 40
lily config animation on
lily config border on
```

`resize` grows and shrinks with one set of arrows: right and down grow the window, left and up shrink it, and its top-left corner stays put. It goes by `step` unless given a distance.

`border on` draws a ring around the focused window. `border_color` takes `RRGGBB` or `RRGGBBAA`, `border_width` is in points (1–20) and `border_radius` (0–40) is how round the corners are. It is off by default. `rule border_radius 0 Zed` gives an app's windows corners of their own.

`focus_follows_mouse on` focuses the window the pointer comes to rest on (macOS brings it to the front too). `mouse_follows_focus on` puts the pointer in the middle of a window focused from the keyboard.

`sketchybar on` tells [SketchyBar](https://github.com/FelixKratz/SketchyBar) when the Space or the focus changes, with the events `lily_space_change` (`$LILY_SPACE`, from 1, and `$LILY_SPACE_LABEL`) and `lily_focus_change` (`$LILY_APP`, `$LILY_WINDOW`):

```
sketchybar --add event lily_space_change \
           --subscribe space.1 lily_space_change
```

Respect [berry](https://berrywm.org/).

## License

MIT
