# AmigaOS LVO names

`amigaos-lvo.tsv` is compact ABI metadata generated from the public
function lists in the AROS module configuration files. It contains only the
module name, LVO number, and function name needed by Copperline's live ROM
symbol resolver. The source checkout and exact revision are recorded in the
TSV header; regenerate it with:

```sh
tools/generate-amigaos-lvos.py /path/to/AROS
```

The derived table is distributed under the AROS Public License 1.1. A copy
is kept in `LICENSE.AROS`; the generator is Copperline code under GPL-3.0-or-
later.
