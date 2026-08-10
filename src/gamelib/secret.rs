// SPDX-License-Identifier: GPL-3.0-or-later

//! A password or auth token, held for as long as it is needed and no
//! longer.
//!
//! Copperline stores neither: the sync dialog asks for a password, trades
//! it for a token, and forgets both when the sync ends. That leaves the
//! window in which they are in memory, and this type is about keeping that
//! window small and free of copies.
//!
//! Three things a plain `String` would get wrong:
//!
//! - **Growing reallocates.** A `String` that outgrows its buffer copies
//!   itself somewhere new and frees the old one *without* clearing it, so
//!   typing a long password can leave several prefixes of it lying in freed
//!   heap. The buffer here is allocated once at its full width and never
//!   grows, so there is only ever one copy.
//! - **Dropping does not clear.** Freed memory keeps its contents until
//!   something else claims it, where a core dump or a later allocation can
//!   still read it. [`Zeroizing`] overwrites on drop with volatile writes
//!   the optimiser is not allowed to elide -- which a hand-written
//!   `buf.fill(0)` on a buffer nothing reads again very much is.
//! - **`Debug` prints it.** Deriving `Debug` anywhere up the tree would put
//!   a password in a log line or a panic message. The one here says
//!   nothing.

use zeroize::Zeroizing;

/// The most characters a credential box accepts. Long enough for any
/// passphrase, and fixed so the buffer behind it never has to grow.
pub const MAX_LEN: usize = 128;

/// A credential: kept in one buffer, wiped when dropped, and never printed.
pub struct Secret {
    /// Always allocated at [`MAX_LEN`] and never grown, so there is one
    /// copy of the text and `Zeroizing` clears all of it.
    text: Zeroizing<String>,
}

impl Secret {
    pub fn new() -> Self {
        Self {
            text: Zeroizing::new(String::with_capacity(MAX_LEN)),
        }
    }

    /// Append a character, up to the width the buffer was made for. Past
    /// that the character is dropped rather than the buffer grown: growing
    /// would copy what is already typed somewhere new and leave it there.
    pub fn push(&mut self, c: char) {
        if self.text.len() + c.len_utf8() <= MAX_LEN {
            self.text.push(c);
        }
    }

    pub fn pop(&mut self) {
        self.text.pop();
    }

    pub fn clear(&mut self) {
        // Zeroizing clears on drop, not on truncate, so wipe what is there
        // before shortening: `String::clear` only moves the length.
        unsafe { self.text.as_mut_vec() }.fill(0);
        self.text.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// How many characters have been typed, for drawing the mask.
    pub fn chars(&self) -> usize {
        self.text.chars().count()
    }

    /// The text itself. Named so that every place a credential leaves this
    /// type is visible in a grep, and so that nothing reaches for it by
    /// accident through `Deref` or `Display`.
    pub fn expose(&self) -> &str {
        &self.text
    }
}

impl Default for Secret {
    fn default() -> Self {
        Self::new()
    }
}

/// Says nothing, so that a credential cannot reach a log line or a panic
/// message through a `Debug` derive somewhere above it.
impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_secret_never_grows_its_buffer() {
        // One allocation, made once: a password typed a character at a time
        // must not leave prefixes of itself in freed heap along the way.
        let mut s = Secret::new();
        let at = s.expose().as_ptr();
        for _ in 0..MAX_LEN {
            s.push('x');
        }
        assert_eq!(s.chars(), MAX_LEN);
        assert_eq!(s.expose().as_ptr(), at, "the buffer moved while typing");

        // And it stops rather than growing.
        s.push('y');
        assert_eq!(s.chars(), MAX_LEN);
        assert_eq!(s.expose().as_ptr(), at);
    }

    #[test]
    fn a_secret_says_nothing_about_itself() {
        let mut s = Secret::new();
        for c in "hunter2".chars() {
            s.push(c);
        }
        let shown = format!("{s:?}");
        assert!(!shown.contains("hunter2"), "Debug printed the secret");
        assert_eq!(shown, "Secret(<redacted>)");
    }

    #[test]
    fn clearing_wipes_rather_than_forgets() {
        let mut s = Secret::new();
        for c in "hunter2".chars() {
            s.push(c);
        }
        let at = s.expose().as_ptr();
        s.clear();
        assert!(s.is_empty());
        // The bytes behind it are gone, not merely out of reach: the buffer
        // is the same one, so this reads what a later `expose` would.
        let behind = unsafe { std::slice::from_raw_parts(at, "hunter2".len()) };
        assert!(
            behind.iter().all(|&b| b == 0),
            "the old text was still there after clear()"
        );
    }
}
