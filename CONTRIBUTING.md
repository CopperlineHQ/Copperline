# Contributing

Copperline models Amiga hardware behaviour. Contributions should describe
the underlying 68000, Agnus, Denise, Paula, CIA, floppy, expansion, or timing
behaviour they change.

Do not add compatibility branches keyed to a game, demo, ROM, disk, config, or
filename. If a workaround is unavoidable, isolate it behind a hardware-derived
condition and add a TODO for the more accurate model that should replace it.

## Which branch

Open fixes for released behaviour against `v1`, the 1.x maintenance branch:
bug fixes, documentation corrections and packaging fixes. They reach `main`
when `v1` is merged forward. Open new features, and anything that changes
existing behaviour, against `main`, the default branch. If you are not sure,
use `main` and say so in the pull request; it can be retargeted. RELEASE.md
("Branches") has the details.

## Assets

Do not commit or attach copyrighted ROMs, disks, hard-disk images, CD images,
or other third-party assets. Local test assets belong in `test-assets/` or in
the repository root, where `.gitignore` excludes the known ROM and disk
extensions.

Bug reports may name the software used to reproduce a problem, but should not
upload the image itself. Prefer a small hardware-focused reproduction when one
can be made.

## Checks

Run the asset-free checks before opening a pull request:

```sh
cargo fmt --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --locked
```

Documentation changes should also build cleanly:

```sh
cd docs
myst build --html --ci --strict --check-links
myst build --pdf --ci --strict
test -s _build/exports/copperline.pdf
```

The ignored integration tests under `tests/` require local ROM and disk
assets. They should skip cleanly when those assets are absent; see
`tests/README.md` for the expected filenames and lookup order.
