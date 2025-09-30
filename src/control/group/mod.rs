cfg_if::cfg_if! {
    // Use the AVX2 implementation if possible: it allows us to scan 32 buckets
    // at once instead of 16. Fall back to SSE2 for 16 buckets, or generic for 8.
    //
    // I attempted an implementation on ARM using NEON instructions, but it
    // turns out that most NEON instructions have multi-cycle latency, which in
    // the end outweighs any gains over the generic implementation.
    if #[cfg(all(
        target_feature = "avx2",
        any(target_arch = "x86", target_arch = "x86_64"),
        not(miri),
    ))] {
        mod avx2;
        use avx2 as imp;
    } else if #[cfg(all(
        target_feature = "sse2",
        any(target_arch = "x86", target_arch = "x86_64"),
        not(miri),
    ))] {
        mod sse2;
        use sse2 as imp;
    } else if #[cfg(all(
        target_arch = "aarch64",
        target_feature = "neon",
        // NEON intrinsics are currently broken on big-endian targets.
        // See https://github.com/rust-lang/stdarch/issues/1484.
        target_endian = "little",
        not(miri),
    ))] {
        mod neon;
        use neon as imp;
    } else if #[cfg(all(
        feature = "nightly",
        target_arch = "loongarch64",
        target_feature = "lsx",
        not(miri),
    ))] {
        mod lsx;
        use lsx as imp;
    } else {
        mod generic;
        use generic as imp;
    }
}
pub(crate) use self::imp::Group;
pub(super) use self::imp::{
    BitMaskWord, NonZeroBitMaskWord, BITMASK_ITER_MASK, BITMASK_MASK, BITMASK_STRIDE,
};
