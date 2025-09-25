cuckoo_hashing_benchmark::aligned_cuckoo_table::HashTable<V>::get:
Lfunc_begin8:
        .cfi_startproc
        ldr x8, [x0, #56]
        eor x8, x8, x1
        mov x10, #30885
        movk x10, #43628, lsl #16
        movk x10, #36300, lsl #32
        movk x10, #11573, lsl #48
        mul x9, x8, x10
        umulh x8, x8, x10
        eor x11, x8, x9
        lsr x13, x11, #57
        
        ldp x9, x12, [x0, #32]


        dup.8b v0, w13
        ldr x8, [x0, #24]
        and x14, x12, x11

        ldr d1, [x8, x14]

        cmeq.8b v1, v1, v0
        fmov x15, d1
        ands x15, x15, #0x8080808080808080
        b.eq LBB8_3
LBB8_1:
        rbit x16, x15
        clz x16, x16
        add x16, x14, x16, lsr #3
        and x16, x9, x16                     <-- wraparound from unalignment, unnecessary
        mvn x16, x16                         <-- negation caused by layout
        lsl x17, x16, #4                     <-- shift by 4 because of paired KVs (layout)

        ldr x17, [x8, x17]
        cmp x17, x1
        b.eq LBB8_6
        sub x16, x15, #1
        ands x15, x16, x15
        b.ne LBB8_1
LBB8_3:
        mul x10, x13, x10
        ror x10, x10, #32
        eor x10, x10, x11
        and x10, x12, x10

        
        ldr d1, [x8, x10]
        cmeq.8b v0, v1, v0
        fmov x11, d0
        ands x11, x11, #0x8080808080808080
        b.eq LBB8_8
LBB8_4:
        rbit x12, x11
        clz x12, x12
        add x12, x10, x12, lsr #3
        and x12, x9, x12
        mvn x16, x12
        lsl x12, x16, #4
        ldr x12, [x8, x12]
        cmp x12, x1
        b.eq LBB8_6
        mov x0, #0
        sub x12, x11, #1
        ands x11, x12, x11
        b.ne LBB8_4
        b LBB8_7
LBB8_6:
        add x8, x8, x16, lsl #4
        add x0, x8, #8
LBB8_7:
        ret
LBB8_8:
        mov x0, #0
        ret