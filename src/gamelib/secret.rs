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
        let was = self.text.len();
        self.text.pop();
        // As many bytes as the character occupied, not one: a password with
        // an accent in it would otherwise leave the rest of that character
        // behind the end of the string.
        self.wipe_tail(was - self.text.len());
    }

    /// Insert at a character index, which is how a caret part-way through
    /// the box types. Bounded exactly as [`push`](Self::push) is.
    ///
    /// Indices rather than a borrowed `&str`, so the text still leaves this
    /// type in only one place: [`expose`](Self::expose).
    pub fn insert_at(&mut self, caret: usize, c: char) {
        if self.text.len() + c.len_utf8() <= MAX_LEN {
            let at = self.byte_of(caret);
            self.text.insert(at, c);
        }
    }

    /// Remove the character at an index. False when the index is past the
    /// end, where there is none.
    pub fn remove_at(&mut self, caret: usize) -> bool {
        let at = self.byte_of(caret);
        if at >= self.text.len() {
            return false;
        }
        let was = self.text.len();
        self.text.remove(at);
        self.wipe_tail(was - self.text.len());
        true
    }

    /// The byte offset a character index lands on, or the end of the text.
    fn byte_of(&self, caret: usize) -> usize {
        self.text
            .char_indices()
            .nth(caret)
            .map_or(self.text.len(), |(at, _)| at)
    }

    /// Zero the bytes a shortening just left behind the end of the string.
    ///
    /// `String::remove` and `String::pop` shift the tail down and move the
    /// length; the characters they dropped are still in the allocation.
    /// `Zeroizing` would get them at drop, but a password deleted mid-typing
    /// should not sit in memory until the dialog closes.
    /// The whole buffer, live bytes and spare capacity together, so a test
    /// can see that what was deleted was actually overwritten.
    ///
    /// # Safety
    /// The caller must not write through it: the string's own invariants
    /// still hold over the first `len` bytes.
    #[cfg(test)]
    pub unsafe fn text_for_test(&mut self) -> &[u8] {
        let vec = unsafe { self.text.as_mut_vec() };
        let (ptr, cap) = (vec.as_ptr(), vec.capacity());
        unsafe { std::slice::from_raw_parts(ptr, cap) }
    }

    fn wipe_tail(&mut self, bytes: usize) {
        let vec = unsafe { self.text.as_mut_vec() };
        for byte in vec.spare_capacity_mut().iter_mut().take(bytes) {
            byte.write(0);
        }
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
    /// Deleting a multi-byte character wipes all of it.
    ///
    /// `pop` used to clear one byte whatever it removed, so a password with
    /// an accent in it left the rest of that character in the spare
    /// capacity until the whole buffer dropped.
    #[test]
    fn popping_a_multibyte_character_wipes_all_of_it() {
        let mut secret = Secret::new();
        for c in "aé".chars() {
            secret.push(c);
        }
        assert_eq!(secret.expose(), "aé");
        let len = secret.expose().len();
        secret.pop();
        assert_eq!(secret.expose(), "a");
        // The two bytes of the character just removed are zero, not just
        // the last of them.
        let now = secret.expose().len();
        let vec = unsafe { secret.text_for_test() };
        assert!(
            vec[now..len].iter().all(|&b| b == 0),
            "part of the deleted character is still there"
        );
    }

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
