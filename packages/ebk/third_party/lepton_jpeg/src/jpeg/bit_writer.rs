/*---------------------------------------------------------------------------------------------
 *  Copyright (c) Microsoft Corporation. All rights reserved.
 *  Licensed under the Apache License, Version 2.0. See LICENSE.txt in the project root for license information.
 *  This software incorporates material from third parties. See NOTICE.txt for details.
 *--------------------------------------------------------------------------------------------*/
// Modified for the EBK project: see EBK-CHANGES.md at the root of this crate.


use std::mem;

use crate::helpers::has_ff;

pub struct BitWriter {
    data_buffer: Vec<u8>,
    fill_register: u64,
    current_bit: u32,
    /// EBK: set once a value was written that does not fit the number of bits given for it - a
    /// coefficient for which the JPEG's Huffman table has no code. Upstream asserts this in debug
    /// builds only and writes other bits in release builds; here the caller turns it into an error.
    misfit: bool,
}

// use to write varying sized bits for coding JPEG. Escapes 0xff -> [0xff,0]
impl BitWriter {
    pub fn new(data_buffer: Vec<u8>) -> Self {
        return BitWriter {
            current_bit: 64,
            fill_register: 0,
            data_buffer,
            misfit: false,
        };
    }

    /// flushes whole bytes from the register into the data buffer
    fn flush_whole_bytes(&mut self) {
        while self.current_bit <= 56 {
            let b = (self.fill_register >> 56) as u8;
            if b != 0xff {
                self.data_buffer.push(b);
            } else {
                // escape 0xff here to avoid multiple scans of the same data
                self.data_buffer.extend_from_slice(&[0xff, 0]);
            }

            self.fill_register <<= 8;
            self.current_bit += 8;
        }
    }

    /// write data
    pub fn write_byte_unescaped(&mut self, b: u8) {
        assert!(self.current_bit == 64);
        self.data_buffer.push(b);
    }

    #[inline(always)]
    pub fn write(&mut self, val: u32, new_bits: u32) {
        /// this is the slow path that is rarely called but generates a lot of code inlined
        /// so we move it out of the main function to keep the main function small with few branches.
        ///
        /// We also call this path when we are about to overflow the buffer to avoid having
        /// to inline the buffer growing logic, which is also much bigger than a simple insert.
        #[inline(never)]
        #[cold]
        fn write_ff_encoded(data_buffer: &mut Vec<u8>, fill_register: u64) {
            for i in 0..8 {
                let b = (fill_register >> (56 - (i * 8))) as u8;
                if b != 0xff {
                    data_buffer.push(b);
                } else {
                    // escape 0xff here to avoid multiple scans of the same data
                    data_buffer.extend_from_slice(&[0xff, 0]);
                }
            }
        }

        self.misfit |= (val as u64) >> new_bits != 0;

        // first see if everything fits in the current register
        if new_bits <= self.current_bit {
            self.fill_register |= (val as u64).wrapping_shl(self.current_bit - new_bits); // support corner case where new_bits is zero, we don't want to panic
            self.current_bit = self.current_bit - new_bits;
        } else {
            // if not, fill up the register so to the 64 bit boundary we can flush it hopefully without any 0xff bytes
            let fill = self.fill_register | (val as u64).wrapping_shr(new_bits - self.current_bit);

            let leftover_new_bits = new_bits - self.current_bit;
            let leftover_val = val & (1 << leftover_new_bits) - 1;

            // flush bytes slowly if we have any 0xff bytes or if we are about to overflow the buffer
            // (overflow check matches implementation in RawVec so that the optimizer can remove the buffer growing code)
            if has_ff(fill)
                || self
                    .data_buffer
                    .capacity()
                    .wrapping_sub(self.data_buffer.len())
                    < 8
            {
                write_ff_encoded(&mut self.data_buffer, fill);
            } else {
                self.data_buffer.extend_from_slice(&fill.to_be_bytes());
            }

            self.fill_register = (leftover_val as u64).wrapping_shl(64 - leftover_new_bits); // support corner case where new_bits is zero, we don't want to panic
            self.current_bit = 64 - leftover_new_bits;
        }
    }

    pub fn pad(&mut self, fillbit: u8) {
        let mut offset = 1;
        while (self.current_bit & 7) != 0 {
            self.write(if (fillbit & offset) != 0 { 1 } else { 0 }, 1);
            offset <<= 1;
        }

        self.flush_whole_bytes();

        debug_assert!(
            self.current_bit == 64,
            "there should be no remainder after padding"
        );
    }

    // flushes the data buffer while escaping all 0xff characters
    pub fn detach_buffer(&mut self) -> Vec<u8> {
        // flush any remaining whole bytes
        self.flush_whole_bytes();

        mem::take(&mut self.data_buffer)
    }

    pub fn ensure_space(&mut self, amount: usize) {
        if self.data_buffer.capacity() < amount {
            let len = self.data_buffer.len();
            self.data_buffer.reserve(amount - len);
        }
    }

    pub fn reset_from_overhang_byte_and_num_bits(&mut self, overhang_byte: u8, num_bits: u32) {
        self.data_buffer.clear();

        self.fill_register = 0;
        self.fill_register = overhang_byte as u64;
        self.fill_register <<= 56;
        self.current_bit = 64 - num_bits;
    }

    /// EBK: whether a value was written that did not fit its number of bits
    pub fn misfit(&self) -> bool {
        self.misfit
    }

    pub fn has_no_remainder(&self) -> bool {
        return self.current_bit == 64;
    }

    pub fn amount_buffered(&self) -> usize {
        self.data_buffer.len()
    }
}
