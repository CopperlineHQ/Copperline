# Serialized board fixture

`copperhf-board-v79.bin` is bincode 1.x serialization of
`BoardDevice::Copperhf(CopperhfBoard::new())`, using the explicit kind ID 13.
It contains an empty board, no ROM, disk image, path, or user data.

The same bytes are decoded and reproduced by
`zorro_device::state::tests::board_fixture_is_independent_of_optional_features`
in default, core-only and MHI-only CI builds. Keep the fixture stable across
feature selections. If the board payload changes, bump `STATE_VERSION` and
regenerate the fixture with the corresponding version name.
