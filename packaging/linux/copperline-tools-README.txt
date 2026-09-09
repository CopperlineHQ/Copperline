Copperline - command-line tools
================================

Companions to the Copperline AppImage, which as a single-entry-point image
cannot carry them itself:

  copperline-ctl         Client for the control protocol (scripting and AI
                         agents), the MCP server mode (--mcp) and the Debug
                         Adapter Protocol adapter (--dap) used by the VS Code
                         extension.
  copperline-import-uae  Converts a WinUAE, Amiberry or FS-UAE config file
                         into a Copperline TOML config:
                         copperline-import-uae --from winuae --in game.uae --out game.toml

Put this directory on PATH, or point the VS Code setting
copperline.ctlExecutable at copperline-ctl.

copperline-ctl launches the emulator when a session needs one. It looks for
it in this order: an explicit path given to the command, the COPPERLINE_BIN
environment variable, a copperline next to copperline-ctl, then PATH. With
the AppImage, set COPPERLINE_BIN to the AppImage's path:

    export COPPERLINE_BIN=/path/to/Copperline-X.Y.Z-x86_64.AppImage

See https://copperline.dev/docs/getting-started/#command-line-tools

Copperline is licensed under GPL-3.0-or-later; see LICENSE.
