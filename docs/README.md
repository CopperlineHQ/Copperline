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
myst build --pdf         # writes docs/_build/exports/copperline.pdf
```

The PDF export collects the chapters listed in `myst.yml` under `exports`.
The individual custom-register reference pages are available in the HTML manual
and embedded debugger help; they are not included in the PDF.

## Validation

Run the same checks as CI before submitting documentation changes:

```sh
cd docs
myst build --html --ci --strict --check-links
myst build --pdf --ci --strict
test -s _build/exports/copperline.pdf
```

The custom-register pages also feed generated Rust data. When editing them,
run `cargo test --profile ci --locked --lib custom` from the repository root
to check their format and control-protocol integration.

## Conventions

- Screenshots live in `docs/images/`. Emulator screenshots are taken with
  deterministic headless runs (`--screenshot-after`), and UI panel images
  with `COPPERLINE_UI_PREVIEW=1 cargo test --release
  panels_render_into_their_rects` (output in `target/ui-preview-*.png`),
  so they can be regenerated exactly. The VS Code walkthroughs use real
  desktop captures under `images/vscode/`; their provenance and recreation
  notes are in `images/vscode/README.md`. These include IDE state and are
  not deterministic framebuffer fixtures.
- Keep the hardware-first rule in prose too: describe hardware behaviour,
  and name software titles only as regression examples.
- Detailed timing rationale lives in `internals/timing.md` and
  `internals/cpu.md`; the guide chapters summarise and link rather than
  duplicate.
