# Terminal display

`--terminal` shows the emulated machine inside the terminal you are already
using and takes its keyboard and mouse from it, instead of opening a window.
No window server is involved, so it works the same over SSH: start Copperline
on a remote build machine and watch and drive the Amiga from your local
terminal.

```sh
copperline --terminal --model A1200 KICK31.ROM workbench.adf
ssh devbox copperline --terminal --noaudio --run build/myprog
```

Everything else on the command line works as it does in a window: the
configuration file, media, `--whdload` and `--run`, the scheduled-input and
capture flags, `--record-input`, save states and warp boot.

## Terminal requirements

The terminal must speak the
[Kitty graphics protocol](https://sw.kovidgoyal.net/kitty/graphics-protocol/)
for the picture and, for full keyboard support, the
[Kitty keyboard protocol](https://sw.kovidgoyal.net/kitty/keyboard-protocol/).
kitty, Ghostty and WezTerm support both. Copperline asks the terminal at start
and stops with an error if it gets no graphics answer. tmux does not pass Kitty
graphics through, so run Copperline outside it.

The keyboard protocol matters because the Amiga keyboard reports every key
going down *and* coming up, and games read held keys. A terminal without the
keyboard protocol only says that a key was typed, so Copperline turns each key
into a short tap: typing works, but keys cannot be held, so keyboard joystick
emulation and games that read held keys do not work. The status line says when
this is the case.

## Picture

Frames are sent at the machine's own resolution, compressed, and scaled by the
terminal to the largest box that fits above the status line. Only frames that
changed are sent. When the connection cannot keep up, Copperline drops frames
instead of slowing the machine down, so emulation speed and sound are unaffected
by a slow link. `--terminal-fps N` caps the frame rate to save bandwidth, for
example on a slow remote link:

```sh
copperline --terminal --terminal-fps 15 game.adf
```

A Workbench screen takes a few kilobytes per frame. A game that scrolls the
whole screen at 50 frames a second needs several megabytes a second; on a slower
link it is shown at a lower frame rate.

## Keyboard, mouse and joysticks

Keys reach the Amiga as in a window: the same keymap (F11 is Help, the Super
keys are the Amiga keys), and the same keyboard joystick bindings from
`[input]`. Copperline uses the physical key the terminal reports, so the
layout set on the Amiga decides which character a key types. Pasted text is
typed on the Amiga keyboard, as with Paste as Keystrokes in the window: the
typing starts about a third of a second after the paste, once the paste
shortcut's own modifier keys are up again, and a single paste is limited to
64 KiB (the status line says when one was cut short).

On a terminal without the keyboard protocol, Alt+`[` and Alt+`O` send the two
bytes that also begin a control sequence. Copperline takes them for the keys
when nothing follows within a twentieth of a second.

The terminal's pointer moves the Amiga mouse by the distance it travels over the
picture, as an uncaptured window does. The terminal cannot capture the pointer,
and the guest applies its own mouse acceleration, so the two pointers drift
apart; pushing the pointer into a corner of the picture usually lines them up
again. Clicks count over the picture only. Gamepads connected to the machine Copperline runs
on work as usual.

When the terminal loses focus, every key and button held on the Amiga is
released, since the terminal will not report their releases.

## Host commands

The Amiga keyboard has no F12, so F12 is the host key. Press F12, then:

| Key | Action |
|---|---|
| `q` | Quit |
| `p` | Pause or resume |
| `w` | Warp on or off |
| `r` | Reset (Ctrl+Amiga+Amiga) |
| `s` | Save a screenshot to the screenshot directory |
| `Esc` | Back, without a command |

The status line at the bottom shows the emulated time, whether the machine is
running, paused or warping, the frame rate and bandwidth of the picture, and
messages such as a saved screenshot's path.

## Ending a session

F12 then `q` is the way out from the keyboard. The terminal is in raw mode
while Copperline owns it, so Ctrl+C and Ctrl+\ are keys for the Amiga, not
interrupts.

A session also ends cleanly when the process is sent SIGTERM, SIGHUP, SIGINT
or SIGQUIT -- by `kill`, by `timeout`, or because the terminal window or SSH
connection closed. Copperline then leaves as it does for a quit and exits
with status 128 plus the signal number (143 for SIGTERM). That holds while
the session is still starting, too: a signal ends it even if the terminal's
output is stopped (Ctrl+S, or a link that has stopped draining), and a
terminal that takes no output for ten seconds at start-up ends the session
with an error. It likewise ends, with an error, when the terminal stops
accepting output or goes away, and when a capture flag has written its
capture.

However the session ends, including after an internal error, Copperline turns
off the modes it turned on (alternate screen, mouse and focus reporting,
bracketed paste, the keyboard protocol), restores the terminal's own mode,
and discards the mouse and key reports the terminal had already sent, so they
do not appear at the shell prompt as typed text. SIGKILL cannot be handled:
after `kill -9`, type `reset` and press Enter to get the terminal back.

## Log output and sound

While Copperline owns the terminal, its log goes to a file instead of the
screen: `copperline-terminal-PID.log` in the temporary directory, or the path
given with `--terminal-log PATH`. The path is printed when the session ends.
The default file is always a new one, readable only by you: if a file of that
name is already there, a number is added to the name instead of the file
being reused. A file named with `--terminal-log` is replaced.

Sound plays on the machine Copperline runs on, as in a window. On a remote
machine with no sound device, pass `--noaudio`.

## Limits

- Unix (macOS and Linux) only.
- The terminal display cannot be combined with `--control`, `--control-gui`,
  `--gdb`, `--gdb-gui`, `--benchmark-until`, `--profile-live-audio` or
  netplay.
- A capture flag such as `--screenshot-after` runs the machine unthrottled
  and ends the session when the capture is written, as a windowless capture
  run does; the terminal shows the run as it goes.
- The window's menus, panels and debugger are not available.
