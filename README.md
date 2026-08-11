# `bintern` - Simple byte slice interning for Rust.

An `Interner` takes a slice of bytes and returns a small, unique handle to it.

All identical strings are mapped to the *exact same* handle, effectively allowing for quick comparisons and copy semantics on the *handle* to map onto those same operations on the underlying bytes, regardless of their size or value.

Said handles are `Key`s, and can be used by the `Interner` to return back a reference to the bytes when needed.

## Examples

```rust
use bintern::Interner;
let mut interner = Interner::<()>::new();
let key = interner.intern(b"Hello, world!");
assert_eq!(
    interner.get(key).as_ref(),
    b"Hello, world!"
);
let key = interner.intern(b"Hello\0\nWorld\0");
assert_eq!(
    interner.get(key).as_ref(),
    b"Hello\0\nWorld\0"
);
```
