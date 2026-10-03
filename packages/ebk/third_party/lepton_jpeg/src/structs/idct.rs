/*---------------------------------------------------------------------------------------------
 *  Copyright (c) Microsoft Corporation. All rights reserved.
 *  Licensed under the Apache License, Version 2.0. See LICENSE.txt in the project root for license information.
 *  This software incorporates material from third parties. See NOTICE.txt for details.
 *--------------------------------------------------------------------------------------------*/
// Modified for the EBK project: see EBK-CHANGES.md at the root of this crate.

use bytemuck::cast;
use wide::{i16x8, i32x8};

use crate::jpeg::block_based_image::AlignedBlock;

const _W1: i32 = 2841; // 2048*sqrt(2)*cos(1*pi/16)
const _W2: i32 = 2676; // 2048*sqrt(2)*cos(2*pi/16)
const _W3: i32 = 2408; // 2048*sqrt(2)*cos(3*pi/16)
const _W5: i32 = 1609; // 2048*sqrt(2)*cos(5*pi/16)
const _W6: i32 = 1108; // 2048*sqrt(2)*cos(6*pi/16)
const _W7: i32 = 565; // 2048*sqrt(2)*cos(7*pi/16)

const W3: i32 = 2408; // 2048*sqrt(2)*cos(3*pi/16)
const W6: i32 = 1108; // 2048*sqrt(2)*cos(6*pi/16)
const W7: i32 = 565; // 2048*sqrt(2)*cos(7*pi/16)

const W1PW7: i32 = _W1 + _W7;
const W1MW7: i32 = _W1 - _W7;
const W2PW6: i32 = _W2 + _W6;
const W2MW6: i32 = _W2 - _W6;
const W3PW5: i32 = _W3 + _W5;
const W3MW5: i32 = _W3 - _W5;

const R2: i32 = 181; // 256/sqrt(2)

#[inline(always)]
pub fn run_idct(block: &[i32x8; 8]) -> AlignedBlock {
    let t = *block;

    let mut xv0 = (t[0] << 11) + 128;
    let mut xv1 = t[1];
    let mut xv2 = t[2];
    let mut xv3 = t[3];
    let mut xv4 = t[4] << 11;
    let mut xv5 = t[5];
    let mut xv6 = t[6];
    let mut xv7 = t[7];

    // Stage 1.
    let mut xv8 = _W7 * (xv1 + xv7);
    xv1 = xv8 + (W1MW7 * xv1);
    xv7 = xv8 - (W1PW7 * xv7);
    xv8 = _W3 * (xv5 + xv3);
    xv5 = xv8 - (W3MW5 * xv5);
    xv3 = xv8 - (W3PW5 * xv3);

    // Stage 2.
    xv8 = xv0 + xv4;
    xv0 -= xv4;
    xv4 = W6 * (xv2 + xv6);
    xv6 = xv4 - (W2PW6 * xv6);
    xv2 = xv4 + (W2MW6 * xv2);
    xv4 = xv1 + xv5;
    xv1 -= xv5;
    xv5 = xv7 + xv3;
    xv7 -= xv3;

    // Stage 3.
    xv3 = xv8 + xv2;
    xv8 -= xv2;
    xv2 = xv0 + xv6;
    xv0 -= xv6;
    xv6 = ((R2 * (xv1 + xv7)) + 128) >> 8;
    xv1 = ((R2 * (xv1 - xv7)) + 128) >> 8;

    // Stage 4.
    let row = [
        (xv3 + xv4) >> 8,
        (xv2 + xv6) >> 8,
        (xv0 + xv1) >> 8,
        (xv8 + xv5) >> 8,
        (xv8 - xv5) >> 8,
        (xv0 - xv1) >> 8,
        (xv2 - xv6) >> 8,
        (xv3 - xv4) >> 8,
    ];

    // transpose and now do vertical
    let [
        mut yv0,
        mut yv1,
        mut yv2,
        mut yv3,
        mut yv4,
        mut yv5,
        mut yv6,
        mut yv7,
    ] = i32x8::transpose(row);

    yv0 = (yv0 << 8) + 8192;
    yv4 = yv4 << 8;

    // Stage 1.
    let mut yv8 = (W7 * (yv1 + yv7)) + 4;
    yv1 = (yv8 + (W1MW7 * yv1)) >> 3;
    yv7 = (yv8 - (W1PW7 * yv7)) >> 3;
    yv8 = (W3 * (yv5 + yv3)) + 4;
    yv5 = (yv8 - (W3MW5 * yv5)) >> 3;
    yv3 = (yv8 - (W3PW5 * yv3)) >> 3;

    // Stage 2.
    yv8 = yv0 + yv4;
    yv0 -= yv4;
    yv4 = ((W6) * (yv2 + yv6)) + 4;
    yv6 = (yv4 - (W2PW6 * yv6)) >> 3;
    yv2 = (yv4 + (W2MW6 * yv2)) >> 3;
    yv4 = yv1 + yv5;
    yv1 -= yv5;
    yv5 = yv7 + yv3;
    yv7 -= yv3;

    // Stage 3.
    yv3 = yv8 + yv2;
    yv8 -= yv2;
    yv2 = yv0 + yv6;
    yv0 -= yv6;
    yv6 = ((R2 * (yv1 + yv7)) + 128) >> 8;
    yv1 = ((R2 * (yv1 - yv7)) + 128) >> 8;

    // Stage 4.
    AlignedBlock::new(cast([
        i16x8::from_i32x8_truncate((yv3 + yv4) >> 11),
        i16x8::from_i32x8_truncate((yv2 + yv6) >> 11),
        i16x8::from_i32x8_truncate((yv0 + yv1) >> 11),
        i16x8::from_i32x8_truncate((yv8 + yv5) >> 11),
        i16x8::from_i32x8_truncate((yv8 - yv5) >> 11),
        i16x8::from_i32x8_truncate((yv0 - yv1) >> 11),
        i16x8::from_i32x8_truncate((yv2 - yv6) >> 11),
        i16x8::from_i32x8_truncate((yv3 - yv4) >> 11),
    ]))
}
