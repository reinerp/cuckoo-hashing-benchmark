use super::super::{BitMask, Tag};
use core::mem;
use core::num::NonZeroU32;

#[cfg(target_arch = "x86")]
use core::arch::x86;
#[cfg(target_arch = "x86_64")]
use core::arch::x86_64 as x86;

pub(crate) type BitMaskWord = u32;
pub(crate) type NonZeroBitMaskWord = NonZeroU32;
pub(crate) const BITMASK_STRIDE: usize = 1;
pub(crate) const BITMASK_MASK: BitMaskWord = 0xffffffff;
pub(crate) const BITMASK_ITER_MASK: BitMaskWord = !0;

/// Abstraction over a group of control tags which can be scanned in
/// parallel.
///
/// This implementation uses a 256-bit AVX2 value.
#[derive(Copy, Clone)]
pub(crate) struct Group(x86::__m256i);

// FIXME: https://github.com/rust-lang/rust-clippy/issues/3859
#[allow(clippy::use_self)]
impl Group {
    /// Number of bytes in the group.
    pub(crate) const WIDTH: usize = mem::size_of::<Self>();

    /// Returns a full group of empty tags, suitable for use as the initial
    /// value for an empty hash table.
    ///
    /// This is guaranteed to be aligned to the group size.
    #[inline]
    #[allow(clippy::items_after_statements)]
    pub(crate) const fn static_empty() -> &'static [Tag; Group::WIDTH] {
        #[repr(C)]
        struct AlignedTags {
            _align: [Group; 0],
            tags: [Tag; Group::WIDTH],
        }
        const ALIGNED_TAGS: AlignedTags = AlignedTags {
            _align: [],
            tags: [Tag::EMPTY; Group::WIDTH],
        };
        &ALIGNED_TAGS.tags
    }

    /// Loads a group of tags starting at the given address.
    #[inline]
    #[allow(clippy::cast_ptr_alignment)] // unaligned load
    pub(crate) unsafe fn load(ptr: *const Tag) -> Self {
        Group(x86::_mm256_loadu_si256(ptr.cast()))
    }

    /// Loads a group of tags starting at the given address, which must be
    /// aligned to `mem::align_of::<Group>()`.
    #[inline]
    #[allow(clippy::cast_ptr_alignment)]
    pub(crate) unsafe fn load_aligned(ptr: *const Tag) -> Self {
        debug_assert_eq!(ptr.align_offset(mem::align_of::<Self>()), 0);
        Group(x86::_mm256_load_si256(ptr.cast()))
    }

    /// Stores the group of tags to the given address, which must be
    /// aligned to `mem::align_of::<Group>()`.
    #[inline]
    #[allow(clippy::cast_ptr_alignment)]
    pub(crate) unsafe fn store_aligned(self, ptr: *mut Tag) {
        debug_assert_eq!(ptr.align_offset(mem::align_of::<Self>()), 0);
        x86::_mm256_store_si256(ptr.cast(), self.0);
    }

    /// Returns a `BitMask` indicating all tags in the group which have
    /// the given value.
    #[inline]
    pub(crate) fn match_tag(self, tag: Tag) -> BitMask {
        #[allow(
            clippy::cast_possible_wrap, // tag.0: Tag as i8
            // tag: i32 as u32
            //   note: _mm256_movemask_epi8 returns a 32-bit mask in a i32, the
            //   upper 32-bits of the i32 are zeroed:
            clippy::cast_sign_loss,
            clippy::cast_possible_truncation
        )]
        unsafe {
            let cmp = x86::_mm256_cmpeq_epi8(self.0, x86::_mm256_set1_epi8(tag.0 as i8));
            BitMask(x86::_mm256_movemask_epi8(cmp) as u32)
        }
    }

    /// Returns a `BitMask` indicating all tags in the group which are
    /// `EMPTY`.
    #[inline]
    pub(crate) fn match_empty(self) -> BitMask {
        self.match_tag(Tag::EMPTY)
    }

    /// Returns a `BitMask` indicating all tags in the group which are
    /// `EMPTY` or `DELETED`.
    #[inline]
    pub(crate) fn match_empty_or_deleted(self) -> BitMask {
        #[allow(
            // tag: i32 as u32
            //   note: _mm256_movemask_epi8 returns a 32-bit mask in a i32, the
            //   upper 32-bits of the i32 are zeroed:
            clippy::cast_sign_loss,
            clippy::cast_possible_truncation
        )]
        unsafe {
            // A tag is EMPTY or DELETED iff the high bit is set
            BitMask(x86::_mm256_movemask_epi8(self.0) as u32)
        }
    }

    /// Returns a `BitMask` indicating all tags in the group which are full.
    #[inline]
    pub(crate) fn match_full(&self) -> BitMask {
        self.match_empty_or_deleted().invert()
    }

    /// Performs the following transformation on all tags in the group:
    /// - `EMPTY => EMPTY`
    /// - `DELETED => EMPTY`
    /// - `FULL => DELETED`
    #[inline]
    pub(crate) fn convert_special_to_empty_and_full_to_deleted(self) -> Self {
        // Map high_bit = 1 (EMPTY or DELETED) to 1111_1111
        // and high_bit = 0 (FULL) to 1000_0000
        //
        // Here's this logic expanded to concrete values:
        //   let special = 0 > tag = 1111_1111 (true) or 0000_0000 (false)
        //   1111_1111 | 1000_0000 = 1111_1111
        //   0000_0000 | 1000_0000 = 1000_0000
        #[allow(
            clippy::cast_possible_wrap, // tag: Tag::DELETED.0 as i8
        )]
        unsafe {
            let zero = x86::_mm256_setzero_si256();
            let special = x86::_mm256_cmpgt_epi8(zero, self.0);
            Group(x86::_mm256_or_si256(
                special,
                x86::_mm256_set1_epi8(Tag::DELETED.0 as i8),
            ))
        }
    }
}