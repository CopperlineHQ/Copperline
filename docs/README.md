# Building the documentation

The documentation under this directory is written in
[MyST Markdown](https://mystmd.org/) and built with the `myst` CLI.

## Dependencies

- [Node.js](https://nodejs.org/) and the MyST CLI:

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

The PDF export collects every chapter into a single document, as listed in
`myst.yml` under `exports`.

## Conventions

- Screenshots live in `docs/images/`. Emulator screenshots are taken with
  deterministic headless runs (`--screenshot-after`), and UI panel images
  with `COPPERLINE_UI_PREVIEW=1 cargo test --release
  panels_render_into_their_rects` (output in `target/ui-preview-*.png`),
  so they can be regenerated exactly.
- Keep the hardware-first rule in prose too: describe hardware behaviour,
  and name software titles only as regression examples.
- Detailed timing rationale lives in `internals/timing.md` and
  `internals/cpu.md`; the guide chapters summarise and link rather than
  duplicate.
