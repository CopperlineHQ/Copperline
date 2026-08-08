# WHDLoad support archives

This directory holds the third-party archives that Copperline's direct
WHDLoad boot (`--whdload`, `[whdload]` in the configuration,
`src/whdload.rs`) unpacks at launch time to stage a boot volume:

- `WHDLoad_usr.lha` -- the unmodified WHDLoad user distribution from
  <https://whdload.de/>. WHDLoad is (C) Bert Jahn; since release 18.2 it is
  freeware (registration is neither required nor possible) and the package
  is distributed free of charge from whdload.de. Copperline redistributes
  the archive unmodified and extracts only `WHDLoad/C/WHDLoad` into the
  staged boot volume.
- `skick346.lha` -- the Soft-Kicker package by Toni Wilen, from Aminet
  (<https://aminet.net/util/boot/skick346.lha>), whose `Kickstarts/*.RTB`
  relocation tables accompany the raw Kickstart images WHDLoad loads into
  expansion memory.

The archives themselves are not committed (this README is); they are
fetched with pinned checksums by `tools/fetch-whdload.sh` for development
and at packaging time for the release bundles, which ship them under this
directory's installed equivalent (`Contents/Resources/whdboot`,
`share/copperline/whdboot`, or `whdboot\` next to the Windows executable).
`COPPERLINE_WHDBOOT_DIR` overrides the search (see
`whdload::find_whdboot_assets`).

Kickstart ROM images are copyrighted and are NEVER part of this directory
or of any Copperline distribution; the booter reads them from the user's
own collection (`[whdload] kickstarts`). A `Kickstarts/` subdirectory
created here by the user is scanned as a convenience.
