# Copperline CPU-core patches

Based on the published `m68k` 0.14.7 crate (MIT license, see LICENSE).
The source is carried here so native and portable builds use the same reviewed
patches without depending on an unpublished upstream revision.

Local changes:

- A batch boundary callback lets the host synchronize hardware before ordinary
  bus accesses and yield when a hardware event or interrupt becomes due.

- Multiple physical direct-memory regions, with read-only fetch/data support,
  atomic permission/range guards, PMMU translation and native trace validation.

Remove the local path dependency once an upstream release provides these APIs.
Test independently with `cargo test --manifest-path vendor/m68k/Cargo.toml
--features serde,jit`; format this crate separately from Copperline.
