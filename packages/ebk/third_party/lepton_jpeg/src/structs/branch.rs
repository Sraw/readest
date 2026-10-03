/*---------------------------------------------------------------------------------------------
 *  Copyright (c) Microsoft Corporation. All rights reserved.
 *  Licensed under the Apache License, Version 2.0. See LICENSE.txt in the project root for license information.
 *  This software incorporates material from third parties. See NOTICE.txt for details.
 *--------------------------------------------------------------------------------------------*/
// Modified for the EBK project: see EBK-CHANGES.md at the root of this crate.

/*
 The logic here is different here than the C++ version, resulting in
 a 2x speed increase. Nothing magic, the main change is to not
 store the probability, since it is deterministically determined
 based on the true/false counts. Instead of doing the calculation,
 we just lookup the 16-bit value in a lookup table to get the
 corresponding probabiity.
*/

pub struct Branch {
    /// The top byte is the number of false bits seen so far
    /// and the bottom byte is the number of true bits seen.
    /// On overflow both values are normalized by dividing by 2 (rounding up).
    ///
    /// Both counts are never less than 1, so we start off with 0x0101.
    counts: u16,
}

impl Default for Branch {
    fn default() -> Branch {
        Branch::new()
    }
}

/// used to precalculate the probabilities and store them as a const array
const fn problookup() -> [u8; 65536] {
    let mut retval = [0; 65536];
    let mut i = 1i32;
    while i < 65536 {
        let a = i >> 8;
        let b = i & 0xff;

        retval[i as usize] = ((a << 8) / (a + b)) as u8;
        i += 1;
    }

    return retval;
}

/// precalculated probabilities for the next bit being false
static PROB_LOOKUP: [u8; 65536] = problookup();

impl Branch {
    pub fn new() -> Self {
        Branch { counts: 0x0101 }
    }

    /// used for debugging to keep the state for hashing
    #[allow(dead_code)]
    pub fn get_u64(&self) -> u64 {
        let c = self.counts;
        return ((PROB_LOOKUP[self.counts as usize] as u64) << 16) + c as u64;
    }

    /// Returns the probability of the next bit being a false as a value between 1 and 255
    ///
    /// Calculated by looking up the probability in a precalculated table
    /// where 'f' is the number of false bits and 't' is the number of true bits seen.
    ///
    /// (f * 256) / (f + t)
    #[inline(always)]
    pub fn get_probability(&self) -> u8 {
        PROB_LOOKUP[self.counts as usize]
    }

    /// Updates the counters when we encounter a 1 or 0. If we hit 255 values, then
    /// we normalize both counts (divide by 2), except in the case where the remaining value is 1,
    /// in which case we don't touch. This biases the probability to get better results
    /// when there are long runs of 1 or 0.
    ///
    /// This function merges updating either the true or false counter
    /// by swapping the top and bottom byte of the 16-bit value.
    ///
    /// The update algorithm looks like this (with top and bottom swapped depending on 'bit'):
    ///
    /// if top_byte < 0xff {
    ///  top_byte += 1;
    /// } else if bottom_byte != 1 {
    ///  top_byte = 0x81;
    ///  bottom_byte = (bottom_byte + 1) >> 1;
    /// }
    #[inline(always)]
    pub fn record_and_update_bit(&mut self, bit: bool) {
        // rotation is used to update either the true or false counter
        // this allows the same code to be used without branching,
        // which makes the CPU about 20% happier.
        //
        // Since the bits are randomly 1/0, the CPU branch predictor does
        // a terrible job and ends up wasting a lot of time. Normally
        // branches are a better idea if the branch very predictable vs
        // this case where it is better to always pay the price of the
        // extra rotation to avoid the branch.
        let orig = self.counts.rotate_left(bit as u32 * 8);
        let (mut sum, o) = orig.overflowing_add(0x100);
        if o {
            // normalize, except in special case where we have 0xff or more same bits in a row
            // in which case we want to bias the probability to get better compression
            //
            // CPU branch prediction soon realizes that this section is not often executed
            // and will optimize for the common case where the counts are not 0xff.
            let mask = if orig == 0xff01 { 0xff00 } else { 0x8100 };

            // upper byte is 0 since we incremented 0xffxx so we don't have to mask it
            sum = ((1 + sum) >> 1) | mask;
        }

        self.counts = sum.rotate_left(bit as u32 * 8);
    }
}
