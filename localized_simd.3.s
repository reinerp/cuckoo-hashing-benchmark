
cuckoo_hashing_benchmark::localized_simd_cuckoo_table::HashTable<V>::get:
Lfunc_begin8:
        .cfi_startproc
        ldr x8, [x0, #32]
        eor x8, x8, x1
        mov x10, #30885
        movk x10, #43628, lsl #16
        movk x10, #36300, lsl #32
        movk x10, #11573, lsl #48
        mul x9, x8, x10
        umulh x8, x8, x10
        eor x11, x8, x9
        lsr x13, x11, #57
        ldr x12, [x0, #16]
        dup.8b v0, w13
        ldr x8, [x0]
        and x9, x12, x11
        add x15, x8, x9
        ldr d1, [x15, #56]
        cmeq.8b v1, v1, v0
        fmov x14, d1
        cbz x14, LBB8_4
        and x16, x14, #0x8080808080808080
LBB8_2:
        cbz x16, LBB8_4
        rbit x14, x16
        clz x14, x14
        lsr x14, x14, #3
        sub x17, x16, #1
        and x16, x17, x16
        ldr x17, [x15, x14, lsl #3]
        cmp x17, x1
        b.eq LBB8_8
        b LBB8_2
LBB8_4:
        mul x9, x13, x10
        ror x9, x9, #32
        eor x9, x9, x11
        and x9, x9, x12
        add x10, x8, x9
        ldr d1, [x10, #56]
        cmeq.8b v0, v1, v0
        fmov x11, d0
        cbz x11, LBB8_9
        and x11, x11, #0x8080808080808080
LBB8_6:
        cbz x11, LBB8_9
        rbit x12, x11
        clz x12, x12
        lsr x14, x12, #3
        sub x12, x11, #1
        and x11, x12, x11
        ldr x12, [x10, x14, lsl #3]
        cmp x12, x1
        b.ne LBB8_6
LBB8_8:
        add x8, x8, x9
        add x8, x8, x14, lsl #3
        add x0, x8, #64
        ret
LBB8_9:
        mov x0, #0
        ret
