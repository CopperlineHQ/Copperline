// SPDX-License-Identifier: GPL-3.0-or-later

//! The WHDLoad game library: a list of the packages you have, what the
//! OpenRetro game database says about them, and the sync that fetches it.
//!
//! Compiled only with the `game-library` feature. It is the one part of
//! Copperline that talks to the internet on its own account, and the only
//! part that needs an HTTP client and a TLS stack, so a build without the
//! feature links neither and carries none of this. WHDLoad itself does not
//! depend on any of it: a package still boots from a path in the
//! configuration exactly the same way.
//!
//! Deliberately self-contained. Nothing here reaches into the emulator, and
//! the rest of Copperline reaches in through a small surface: the launcher
//! asks for the list, for one game's details, and for a sync to be run.

pub mod cover;
pub mod db;
pub mod http;
pub mod library;
pub mod openretro;
pub mod scan;
pub mod secret;
pub mod sha1;
pub mod support;

pub use cover::Covers;
pub use db::{Catalogue, Database, Game, Known};
pub use library::{Entry, Library};
pub use scan::{Progress, Scan};
pub use secret::Secret;
