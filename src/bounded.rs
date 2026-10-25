//! Inputs that the Kani proofs share, built only under `cargo kani`.

/// Any `N` bytes and any length up to `N`, so `&bytes[..len]` stands for each input up to `N`
/// bytes long.
pub(crate) fn bytes<const N: usize>() -> ([u8; N], usize) {
    let len: usize = kani::any();
    kani::assume(len <= N);
    (kani::any(), len)
}
