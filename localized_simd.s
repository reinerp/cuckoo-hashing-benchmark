cuckoo_hashing_benchmark::localized_simd_cuckoo_table::HashTable<V>::get:
Lfunc_begin10:
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
        ldr x8, [x0]  <-- non-contiguous load from base struct

        dup.8b v0, w13
        and x9, x12, x11

        add x15, x8, x9, lsl #7         <-- multiply by 128; avoided in the other case by applying mask correctly
        ldr d1, [x15]

        cmeq.8b v1, v1, v0
        fmov x14, d1
        ands x14, x14, #0x8080808080808080   <--- this isn't needed in the find_miss case
        b.eq LBB10_4
        add x16, x15, #8                <-- extra add 8
LBB10_2:
        rbit x15, x14                   <-- we could drop this on AArch64 by counting backwards not forwards
        clz x15, x15
        lsr x15, x15, #3                <-- we could avoid this by relying on correct stride



        ldr x17, [x16, x15, lsl #3]
        cmp x17, x1
        b.eq LBB10_9
        sub x15, x14, #1
        ands x14, x15, x14
        b.ne LBB10_2
LBB10_4:
        mul x9, x13, x10
        ror x9, x9, #32
        eor x9, x9, x11
        and x9, x9, x12
        add x11, x8, x9, lsl #7        <-- multiply by 128, could be dropped

        ldr d1, [x11]
        cmeq.8b v0, v1, v0
        fmov x10, d0
        ands x10, x10, #0x8080808080808080
        b.eq LBB10_10
        add x11, x11, #8               <-- extra add 8
LBB10_6:
        rbit x12, x10
        clz x12, x12
        lsr x15, x12, #3
        ldr x12, [x11, x15, lsl #3]
        cmp x12, x1
        b.eq LBB10_9
        mov x0, #0
        sub x12, x10, #1
        ands x10, x12, x10
        b.ne LBB10_6
        ret
LBB10_9:
        add x8, x8, x9, lsl #7
        add x8, x8, x15, lsl #3
        add x0, x8, #64
        ret
LBB10_10:
        mov x0, #0
        ret