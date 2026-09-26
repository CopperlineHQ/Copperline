# Building the documentation

The documentation under this directory is written in
[MyST Markdown](https://mystmd.org/) and built with the `myst` CLI.

## Dependencies

- [Node.js](https://nodejs.org/) (CI uses Node 24) and the MyST CLI:

  ```sh
  npm install -g mystmd
  ```

- For PDF output only: [Typst](https://typst.app/), which MyST uses as the
  PDF renderer:

  ```sh
  brew install typst        # macOS
  # or: cargo install typst-cli
  # or a release binary from https://github.com/typst/typst/releases
  # (CI installs it with typst-community/setup-typst)
  ```

  The first PDF build also downloads the MyST Typst template, so it needs
  network access once.

## HTML

```sh
cd docs
myst build --html        # static site in docs/_build/html
myst start               # or: live-reloading local preview server
```

The HTML site is themed to match copperline.dev via `site.css` (wired up
through the `style` option in `myst.yml`). On every `v*` tag the
`docs-site.yml` workflow rebuilds it with `BASE_URL=/docs` and publishes it
to the website repository, where it is served at
[copperline.dev/docs](https://copperline.dev/docs). The `@font-face` rules
in `site.css` point at fonts hosted by the website, so local previews fall
back to system fonts; everything else looks the same.

## PDF

```sh
cd docs
python3 ../tools/build-docs-pdf.py  # writes docs/_build/exports/copperline.pdf
```

The PDF export collects the chapters listed in `myst.yml` under `exports`.
The individual custom-register reference pages are available in the HTML manual
and embedded debugger help; they are not included in the PDF.

The helper runs `myst build --pdf` (passing on its own arguments, such as
`--ci --strict`), rewrites the generated Typst sources to use native Typst
tables, and compiles them with `typst`. This avoids the legacy `tablex`
helper's layout-convergence failures and incorrect page counts in long
manuals. It also corrects menu arrows emitted as literal `arrow.r` text by
MyST 1.10. Markdown sources and the downloaded templates are left unchanged.
New tablex features fail the build until an equivalent native mapping is added.

## Validation

Run the same checks as CI before submitting documentation changes:

```sh
cd docs
myst build --html --ci --strict --check-links
python3 ../tools/build-docs-pdf.py --ci --strict
test -s _build/exports/copperline.pdf
```

The custom-register pages also feed generated Rust data. When editing them,
run `cargo test --profile ci --locked --lib custom` from the repository root
to check their format and control-protocol integration.

## Conventions

- Screenshots live in `docs/images/`. Emulator screenshots are taken with
  deterministic headless runs (`--screenshot-after`). Software UI images
  come from the UI tests run with `COPPERLINE_UI_PREVIEW=1` (output in
  `target/ui-preview-*.png`): `cargo test --release
  panels_render_into_their_rects` for the panels, and the MT-32,
  Coppersynth and keyboard panel tests for those strips. The egui inspector
  images come from the ignored GPU preview tests listed in
  `internals/video.md`, which write to `target/egui-debugger/`. All of
  them can be regenerated rather than captured by hand. The VS Code
  walkthroughs are the exception: real desktop captures under
  `images/vscode/`, with provenance and recreation notes in
  `images/vscode/README.md`. They include IDE state and are not
  deterministic framebuffer fixtures.
- Keep the hardware-first rule in prose too: describe hardware behaviour,
  and name software titles only as regression examples.
- Detailed timing rationale lives in `internals/timing.md` and
  `internals/cpu.md`; the guide chapters summarize and link rather than
  duplicate.
