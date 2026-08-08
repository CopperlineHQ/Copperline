# WHDLoad test fixtures

Project-owned fixtures for the WHDLoad booter (`src/whdload.rs`); no
third-party game content is involved.

- `testgame.asm` -- a minimal WHDLoad slave: once WHDLoad hands over
  control it disables DMA and interrupts, writes COLOR00 = $0B4, and
  parks, so a booted frame is a solid teal screen that a screenshot test
  can assert on.
- `Test.Slave` -- the assembled slave (committed so the tests need no
  m68k toolchain):

  ```sh
  vasmm68k_mot -Fhunkexe -nosym -o Test.Slave testgame.asm
  ```

- `TestGame.lha` -- `TestGame/Test.Slave` packed as a stored (`-lh0-`)
  LhA archive, the shape a real WHDLoad package arrives in:

  ```sh
  python3 mklha.py TestGame.lha "TestGame/Test.Slave=Test.Slave"
  ```

The slave header is also the reference for the `parse_slave` unit tests in
`src/whdload.rs`. The end-to-end boot lives in `tests/whdload_boot.rs`; it
needs the fetched support archives (`tools/fetch-whdload.sh`) and a local
Kickstart 3.1 (40.068 A1200) image, and skips cleanly without them.
