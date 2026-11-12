# lilypad

A floating window manager for macOS with smooth animations.

No tiling: windows stay where you put them. SIP stays on.

## Install

```sh
git clone https://github.com/emreekici3/lilypad.git
cd lilypad
cargo install --path . --root ~
mkdir -p ~/.config/lilypad
cp examples/lilyrc ~/.config/lilypad/lilyrc
lily install && lily start
```

Put `~/bin` on your `PATH` and allow `lily` under Privacy & Security → Accessibility. `lily doctor` checks the rest.

## Usage

```sh
lily place max            # center, left, right, top, bottom, top-left, …, max
lily place center 70%
lily snap left            # slide to the next edge or window
lily move up              # and resize, focus: left, right, up, down
lily grid 2:2:1:0:1:2     # rows:cols:x:y:w:h, here the right half
lily space 2              # also next, prev, recent, or a label
lily send 3               # take the window there, and go with it
lily scratchpad toggle term
```

Hold `alt` and drag to move a window, right-drag to resize it.

## Config

`~/.config/lilypad/lilyrc` is a shell script, run at start. See [`examples/lilyrc`](examples/lilyrc).

```sh
lily config gap 12
lily config animation on
lily config border on
lily config border_color 8aa9f8

lily bind 'alt - m : lily place max'
lily label 2 web
lily rule space web Safari --follow
lily rule grid 1:2:0:0:1:1 Zed
lily scratchpad term kitty
```

Settings: `gap`, `step`, `animation`, `border`, `border_color`, `border_width`, `border_radius`, `focus_follows_mouse`, `mouse_follows_focus`, `mouse_modifier`, `sketchybar`. Run `lily help` for all commands.

## License

MIT
