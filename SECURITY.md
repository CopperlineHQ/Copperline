# Security Policy

Report security issues through GitHub private vulnerability reporting once the
public `CopperlineHQ/Copperline` repository is available. If that is not enabled,
open a public issue asking for a private contact path and omit exploit details.

Do not attach copyrighted ROMs, disks, hard-disk images, CD images, or other
third-party assets to a report. A minimal config, log excerpt, crash backtrace,
or synthetic reproducer is preferred.

Copperline is an emulator and should not normally process untrusted media in a
privileged context. Treat crashes or malformed-image panics as security issues
only when they cross a meaningful trust boundary, such as arbitrary code
execution on the host or writes outside the selected local files.
