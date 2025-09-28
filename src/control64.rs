use cfg_if::cfg_if;

#[inline(always)]
pub fn search(key: u64, bucket: [u64; 4]) -> Option<usize> {
    cfg_if! {
        if #[cfg(all(target_arch = "aarch64", target_feature = "neon"))] {
            return {
                use core::arch::aarch64::*;
                unsafe {
                    let bucket_ptr = bucket.as_ptr();
                    let key: uint64x2_t = vdupq_n_u64(key);
                    let bucket0: uint64x2_t = vld1q_u64(bucket_ptr);
                    let bucket1: uint64x2_t = vld1q_u64(bucket_ptr.add(2));
                    let eq0: uint64x2_t = vceqq_u64(bucket0, key);
                    let eq1: uint64x2_t = vceqq_u64(bucket1, key);
                    let eq0: uint8x16_t = vreinterpretq_u8_u64(eq0);
                    let eq1: uint8x16_t = vreinterpretq_u8_u64(eq1);
                    let mask: uint8x16_t = std::mem::transmute([0u8, 8, 16, 24, !0, !0, !0, !0, !0, !0, !0, !0, !0, !0, !0, !0]);
                    let eq_by_byte: uint8x16_t = vqtbl2q_u8(uint8x16x2_t(eq0, eq1), mask);
                    let eq_by_byte: u64 = vgetq_lane_u64(vreinterpretq_u64_u8(eq_by_byte), 0);
                    if eq_by_byte == 0 {
                        None
                    } else {
                        Some(eq_by_byte.trailing_zeros() as usize / 8)
                    }
                }
            };
        } else if #[cfg(all(target_arch = "x86_64", target_feature = "avx2"))] {
            return {
                use core::arch::x86_64::*;
                unsafe {
                    let key_vec = _mm256_set1_epi64x(key as i64);
                    let bucket_vec = _mm256_loadu_si256(bucket.as_ptr() as *const __m256i);
                    let eq_mask = _mm256_cmpeq_epi64(bucket_vec, key_vec);
                    let movemask = _mm256_movemask_pd(_mm256_castsi256_pd(eq_mask));
                    if movemask == 0 {
                        None
                    } else {
                        Some(movemask.trailing_zeros() as usize)
                    }
                }
            };
        } else {
            unimplemented!()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_search_existing_key() {
        let bucket = [1, 2, 3, 4];
        assert_eq!(search(2, bucket), Some(1));
        assert_eq!(search(1, bucket), Some(0));
        assert_eq!(search(4, bucket), Some(3));
    }

    #[test]
    fn test_search_non_existing_key() {
        let bucket = [1, 2, 3, 4];
        assert_eq!(search(5, bucket), None);
        assert_eq!(search(0, bucket), None);
    }

    #[test]
    fn test_search_zero_empty_slot() {
        let bucket = [1, 0, 3, 4];
        assert_eq!(search(0, bucket), Some(1));

        let empty_bucket = [0, 0, 0, 0];
        assert_eq!(search(0, empty_bucket), Some(0));
    }

    #[test]
    fn test_search_duplicate_keys() {
        let bucket = [5, 5, 3, 5];
        // Should return the first occurrence
        assert_eq!(search(5, bucket), Some(0));
    }
}
