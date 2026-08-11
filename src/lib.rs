//! # `bintern` - Simple byte slice interning for Rust.
//!
//! An interner takes a string of bytes and returns a canonical handle to it (local to the interner).
//!
//! See [`Interner`] for more documentation.

use std::{
    hash::{BuildHasher, Hash, Hasher},
    marker::PhantomData,
};

/// An [`Interner`] key.
///
/// This type has an associated brand type [`B`], which can help with enforcing type-safety and not mixing key types at compile-time. It is purely a marker and has no effect other than on the type system.
///
/// This key should only ever be used with the [`Interner`] it was created with, and is unable to be constructed without an [`Interner`].
///
/// The key supports the SBO (Small Buffer Optimisation), allowing strings <= 7 bytes in length (on 64-bit platforms) to be stored inline with the key, not needing to be allocated in the interner itself.
pub struct Key<B> {
    // Aligned to [`Header`], so we can use the 3 bits that are 0 when aligned for the SBO.
    internal: usize,
    _marker: PhantomData<fn() -> B>,
}

impl<B> std::fmt::Debug for Key<B> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Key")
            .field(&self.internal)
            .finish_non_exhaustive()
    }
}

impl<B> Clone for Key<B> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<B> Copy for Key<B> {}

impl<B> PartialEq for Key<B> {
    fn eq(&self, other: &Self) -> bool {
        self.internal == other.internal
    }
}

impl<B> Eq for Key<B> {}

impl<B> PartialOrd for Key<B> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl<B> Ord for Key<B> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.internal.cmp(&other.internal)
    }
}

impl<B> Hash for Key<B> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.internal.hash(state);
    }
}

/// We use the first byte for storing length and the rest for the data.
const SBO_LEN: usize = size_of::<usize>() - 1;
/// Number of bits it would take to store the length of [`SBO_LEN`] bits.
const SBO_BITS: usize = (SBO_LEN - 1).ilog2() as usize + 1;
/// Mask for the bits that comprise the length of the SBO, if it exists.
//
// This neatly fits into the alignment bits on 64-bit systems, but may break on 128-bit systems? Need to either increase the alignment of [`Header`] or clamp the SBO length.
const SBO_BITS_MASK: u8 = !(u8::MAX << SBO_BITS);

// Make sure the header align is big enough to fit the SBO bits.
const _: () = const { assert!((1usize << SBO_BITS) - 1 < align_of::<Header>()) };

type SBOBuffer = [u8; SBO_LEN];

impl<B> Key<B> {
    pub(crate) fn from_byte_offset(offset: usize) -> Self {
        assert!(offset.is_multiple_of(align_of::<Header>()));

        Self {
            internal: offset,
            _marker: PhantomData,
        }
    }

    /// Create a key from some bytes for SBO.
    ///
    /// # Panics
    /// - If bytes is empty
    /// - If bytes is longer than the supported SBO length
    pub(crate) fn from_bytes(bytes: &[u8]) -> Self {
        assert!(!bytes.is_empty());
        assert!(bytes.len() <= SBO_LEN);

        let end = 1 + bytes.len();

        let mut encoded = 0usize.to_le_bytes();
        encoded[1..end].copy_from_slice(bytes);

        encoded[0] = u8::try_from(bytes.len()).expect("SBO byte length must fit in u8");

        Self {
            internal: usize::from_le_bytes(encoded),
            _marker: PhantomData,
        }
    }

    pub(crate) const fn get(self) -> KeyCow {
        if self.internal & SBO_BITS_MASK as usize != 0 {
            let [flags, bytes @ ..] = self.internal.to_le_bytes();

            let len = flags & SBO_BITS_MASK;

            KeyCow::Inline { len, bytes }
        } else {
            KeyCow::ByteOffset(self.internal)
        }
    }
}

#[derive(Debug, PartialEq)]
pub(crate) enum KeyCow {
    // I don't like this. This should be a direct &[u8]. This should allow an iterator to work over slices instead of AsRef constructions.
    Inline { len: u8, bytes: SBOBuffer },
    ByteOffset(usize),
}

#[repr(transparent)]
#[derive(Debug, Clone)]
struct Header {
    size: usize,
}

/// A container that can intern bytes into unique handles.
///
/// An [`Interner`] takes a slice of bytes and returns a small, unique handle to it.
///
/// All identical strings are mapped to the *exact same* handle, effectively allowing for quick comparisons and copy semantics on the *handle* to map onto those same operations on the underlying bytes, regardless of their size or value.
///
/// Said handles are [`Key`]s, and can be used by the [`Interner`] to return back a reference to the bytes when needed.
///
/// # Examples
///
/// ```rust
/// use bintern::Interner;
///
/// let mut interner = Interner::<()>::new();
///
/// let key = interner.intern(b"Hello, world!");
/// assert_eq!(
///     interner.get(key).as_ref(),
///     b"Hello, world!"
/// );
///
/// let key = interner.intern(b"Hello\0\nWorld\0");
/// assert_eq!(
///     interner.get(key).as_ref(),
///     b"Hello\0\nWorld\0"
/// );
/// ```
// Do we need clone here? It shouldn't be an operation done very frequently.
#[derive(Debug, Clone)]
pub struct Interner<B> {
    // Only used for aligned values, really should be a `Vec<u8>`.
    buffer: Vec<Header>,
    hash: hashbrown::DefaultHashBuilder,
    set: hashbrown::HashTable<Key<B>>,
}

impl<B> Default for Interner<B> {
    fn default() -> Self {
        Self::new()
    }
}

impl<B> Interner<B> {
    /// Create a new [`Interner`].
    ///
    /// This uses hashbrown's default fold hasher.
    #[must_use]
    pub fn new() -> Self {
        Self {
            buffer: Vec::new(),
            hash: hashbrown::DefaultHashBuilder::default(),
            set: hashbrown::HashTable::new(),
        }
    }

    fn get_internal(buffer: &[Header], key: Key<B>) -> impl AsRef<[u8]> {
        enum BufferCow<'a> {
            Inline { len: u8, bytes: [u8; 7] },
            Ref(&'a [u8]),
        }
        impl AsRef<[u8]> for BufferCow<'_> {
            fn as_ref(&self) -> &[u8] {
                match self {
                    BufferCow::Inline { len, bytes } => &bytes[..*len as usize],
                    BufferCow::Ref(items) => items,
                }
            }
        }

        match key.get() {
            KeyCow::Inline { len, bytes } => BufferCow::Inline { len, bytes },
            KeyCow::ByteOffset(offset) => unsafe {
                let start = buffer.as_ptr();
                let keyed = start.byte_add(offset);
                let len = keyed.as_ref().unwrap().size;
                let byte_start = keyed.add(1).cast::<u8>();

                BufferCow::Ref(std::slice::from_raw_parts(byte_start, len))
            },
        }
    }

    /// Intern some bytes, returning a key.
    ///
    /// This doesn't allocate if the key is below the SBO threshold.
    pub fn intern(&mut self, bytes: &[u8]) -> Key<B> {
        let Self { buffer, hash, set } = self;

        let hashed = hash.hash_one(bytes);

        *set.entry(
            hashed,
            |key| Self::get_internal(buffer, *key).as_ref() == bytes,
            |key| hash.hash_one(Self::get_internal(buffer, *key).as_ref()),
        )
        .or_insert_with(|| {
            let len = bytes.len();

            if len != 0 && len <= SBO_LEN {
                return Key::from_bytes(bytes);
            }

            let header = Header { size: len };
            // We fit the bytes into the same buffer as the headers (just makes things aligned automatically), so to reserve we need to find how many header-sized chunks the string would take.
            let num_byte_chunks = bytes.len().div_ceil(size_of::<Header>());

            buffer.reserve(num_byte_chunks + 1);
            buffer.push(header);

            let start = buffer.len();
            buffer.resize(buffer.len() + num_byte_chunks, Header { size: 0 });
            unsafe {
                let writer = buffer.as_mut_ptr().add(start).cast::<u8>();
                writer.copy_from(bytes.as_ptr(), bytes.len());
            }

            Key::from_byte_offset((start - 1) * size_of::<Header>())
        })
        .get()
    }

    /// Get the underlying bytes that a key represents.
    #[must_use]
    pub fn get(&self, key: Key<B>) -> impl AsRef<[u8]> {
        Self::get_internal(&self.buffer, key)
    }

    /// Clear the contents of the interner.
    ///
    /// This invalidates all [`Key`]s that this interner has previously produced.
    pub fn clear(&mut self) {
        self.buffer.clear();
        self.set.clear();
    }
}

#[cfg(test)]
mod tests {
    use std::assert_matches;

    use super::*;

    #[test]
    fn key_from_offset() {
        let key = Key::<()>::from_byte_offset(0);
        assert_eq!(key.internal & SBO_LEN, 0);
        assert_matches!(key.get(), KeyCow::ByteOffset(0),);
        let key = Key::<()>::from_byte_offset(8);
        assert_eq!(key.internal & SBO_LEN, 0);
        assert_matches!(key.get(), KeyCow::ByteOffset(8),);
    }

    #[test]
    #[should_panic(expected = "offset.is_multiple_of(align_of::<Header>())")]
    fn key_from_unaligned_offset() {
        let _ = Key::<()>::from_byte_offset(1);
    }

    #[test]
    fn key_from_bytes() {
        let fill: SBOBuffer = std::array::repeat(u8::MAX - 1);
        let key = Key::<()>::from_bytes(&fill);
        assert_eq!(key.internal & SBO_LEN, 7);
        assert_eq!(
            key.internal,
            usize::from_le_bytes([
                7,
                u8::MAX - 1,
                u8::MAX - 1,
                u8::MAX - 1,
                u8::MAX - 1,
                u8::MAX - 1,
                u8::MAX - 1,
                u8::MAX - 1,
            ])
        );
        assert_matches!(key.get(), KeyCow::Inline { len: 7, .. });
    }

    #[test]
    #[should_panic(expected = "bytes.len() <= SBO_LEN")]
    fn key_from_many_bytes() {
        let _ = Key::<()>::from_bytes(&[0; 100]);
    }

    #[test]
    #[should_panic(expected = "!bytes.is_empty()")]
    fn key_from_empty_bytes() {
        let _ = Key::<()>::from_bytes(&[]);
    }

    #[test]
    fn intern_string() {
        let mut interner = Interner::<()>::new();

        let key = interner.intern(b"hello");
        assert_eq!(
            key.get(),
            KeyCow::Inline {
                len: 5,
                bytes: *b"hello\0\0"
            }
        );

        assert_eq!(interner.get(key).as_ref(), b"hello");

        assert!(interner.buffer.is_empty());

        let key = interner.intern(b"According to all known laws of aviation, there is no way a bee should be able to fly.\nIts wings are too small to get its fat little body off the ground.");
        assert_eq!(key.get(), KeyCow::ByteOffset(0));

        let key = interner.intern(b"Lorem ipsum dolores sit amet");
        assert_eq!(key.get(), KeyCow::ByteOffset(160));
    }
}
