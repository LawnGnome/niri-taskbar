# Niri Taskbar (for Waybar)

This provides a [Waybar][waybar] taskbar for [Niri][niri].

The main shift from the builtin `wlr/taskbar` module is that windows are always
ordered by workspace index, then window ID (which essentially means that the
windows are ordered by creation time, at least as of Waybar 0.12.0).

![Example screenshot](images/screenshot.png)

## Installation

At the moment, this needs to built from source. (If you do package it for a
distro, let me know and I'll update the README.)

### Requirements

- Rust 1.85.0 or later
- Gtk+ 3 (including the development package on distros that separate those out)
- Waybar 0.12.0 (or any version that's API compatible with 0.12, which will
  _probably_ include later versions, but I have no actual knowledge there)

### Building

The standard Rust build process should work fine:

```bash
$ cargo build --release
```

This will give you a shared library module at
`target/release/libniri_taskbar.so`. Feel free to move that wherever makes
sense.

## Configuration

This uses the normal configuration for a [CFFI Waybar module][cffi], which in
practice will look something like this:

```jsonc
{
  "modules-left": ["cffi/niri-taskbar"],
  // ...
  "cffi/niri-taskbar": {
    "module-path": "/your/path/to/libniri_taskbar.so",
  },
}
```

### Application highlighting

Since Niri doesn't currently support urgency notifications, you can highlight
applications based on their app ID and title by configuring application rules in
the Waybar configuration.

For example, to highlight a Signal window that starts with `(1)` (or any other
number), indicating pending notifications, you could configure the taskbar like
so:

```jsonc
{
  "cffi/niri-taskbar": {
    // module-path
    "apps": {
      "signal": [
        {
          "match": "\\([0-9]+\\)$",
          "class": "unread",
        },
      ],
    },
  },
}
```

Each key within the `apps` object is a Wayland app ID, which can have one or
more rules set within it. Each rule must have a `match`, which is a regex that
will be matched against the window title, and a `class`, which is a CSS class
that will be added to the button element if the regex matches.

If more than one rule matches for a single app ID, all matching classes will be
added.

The easiest way to get the app ID for a window is to ask Niri with `niri msg
windows`. Note that app IDs are case sensitive.

### Notifications

You can enable the `notifications` configuration option to have the taskbar
listen to notifications and attempt to highlight the app that sent the
notification.

Configuration wise:

```jsonc
{
  "cffi/niri-taskbar": {
    // other settings
    "notifications": true,
  },
}
```

Highlighted buttons will gain the `.urgent` CSS class. Default styling is
included, but can be overridden [as described below](#styling).

## Styling

The taskbar uses [the same Gtk styling mechanism as Waybar][style]. The top
level taskbar element is given the class `.niri-taskbar`, and contains `button`
elements within it. The only CSS class that is applied by default is the
`focused` class, which is added to the button for the currently focused window.

The default styling assumes a dark background. It provides a basic hover
effect, and highlights the focused window.

For a light background, something like this is likely good:

```css
.niri-taskbar button:hover {
  background: rgba(0, 0, 0, 0.5);
}

.niri-taskbar button.focused {
  background: rgba(0, 0, 0, 0.3);
}
```

If you apply custom CSS classes using application rules as described above,
then those can be styled in the same way. For instance, with the `unread` class
demonstrated above, you could add a border highlight like so:

```css
.niri-taskbar button {
  /* This is useful to prevent the icon being resized when there's no unread. */
  border-bottom: solid 3px transparent;
}

.niri-taskbar button.unread {
  border-bottom: solid 3px white;
}
```

[cffi]: https://github.com/Alexays/Waybar/wiki/Module:-CFFI
[niri]: https://github.com/YaLTeR/niri
[style]: https://github.com/Alexays/Waybar/wiki/Styling
[waybar]: https://github.com/Alexays/Waybar
