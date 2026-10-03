/*---------------------------------------------------------------------------------------------
 *  Copyright (c) Microsoft Corporation. All rights reserved.
 *  Licensed under the Apache License, Version 2.0. See LICENSE.txt in the project root for license information.
 *  This software incorporates material from third parties. See NOTICE.txt for details.
 *--------------------------------------------------------------------------------------------*/
// Modified for the EBK project: see EBK-CHANGES.md at the root of this crate.

use std::io::{Read, Result};

use byteorder::{LittleEndian, ReadBytesExt};

use crate::consts::COLOR_CHANNEL_NUM_BLOCK_TYPES;

#[derive(Debug, Clone, PartialEq)]
pub struct ThreadHandoff {
    pub luma_y_start: u32,
    pub luma_y_end: u32,
    pub segment_offset_in_file: u32,
    pub segment_size: u32,
    pub overhang_byte: u8,
    pub num_overhang_bits: u8,
    pub last_dc: [i16; 4],
}

impl ThreadHandoff {
    pub fn deserialize<R: Read>(num_threads: u8, data: &mut R) -> Result<Vec<ThreadHandoff>> {
        let mut retval: Vec<ThreadHandoff> = Vec::with_capacity(num_threads as usize);

        for _i in 0..num_threads {
            let mut th = ThreadHandoff {
                luma_y_start: data.read_u16::<LittleEndian>()? as u32,
                luma_y_end: 0,             // filled in later
                segment_offset_in_file: 0, // not serialized
                segment_size: data.read_u32::<LittleEndian>()?,
                overhang_byte: data.read_u8()?,
                num_overhang_bits: data.read_u8()?,
                last_dc: [0; 4],
            };

            for j in 0..COLOR_CHANNEL_NUM_BLOCK_TYPES {
                th.last_dc[j] = data.read_i16::<LittleEndian>()?
            }
            for _j in COLOR_CHANNEL_NUM_BLOCK_TYPES..4 {
                data.read_u16::<LittleEndian>()?;
            }

            // EBK: these are bits of one byte; the bit writer subtracts the count from 64
            if th.num_overhang_bits > 8 {
                return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "more than 8 overhang bits"));
            }

            retval.push(th);
        }

        for i in 1..retval.len() {
            retval[i - 1].luma_y_end = retval[i].luma_y_start;
        }

        // last LumaYEnd is not serialzed, filled in later
        return Ok(retval);
    }
}
