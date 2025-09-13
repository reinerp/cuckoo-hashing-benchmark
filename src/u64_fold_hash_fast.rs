#[inline(always)]
pub fn fold_hash_fast(mut key: u64, seed: u64) -> u64 {
    const FOLD: u64 = 0x2d35_8dcc_aa6c_78a5;
    key ^= seed;
    let r = (key as u128) * FOLD as u128;
    ((r >> 64) as u64) ^ (r as u64)
}