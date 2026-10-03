/*---------------------------------------------------------------------------------------------
 *  Copyright (c) Microsoft Corporation. All rights reserved.
 *  Licensed under the Apache License, Version 2.0. See LICENSE.txt in the project root for license information.
 *  This software incorporates material from third parties. See NOTICE.txt for details.
 *--------------------------------------------------------------------------------------------*/
// Modified for the EBK project: see EBK-CHANGES.md at the root of this crate.

use crate::consts::{JpegDecodeStatus, JpegType};

use super::jpeg_header::JpegHeader;

/// used to keep track of position while encoding or decoding a jpeg
pub struct JpegPositionState {
    /// current component
    cmp: usize,

    /// current minimum coded unit (a fraction of dpos)
    mcu: u32,

    /// index of component
    csc: usize,

    /// offset within mcu
    sub: u32,

    /// current block position in image for this component
    dpos: u32,

    /// number of blocks left until reset interval
    rstw: u32,

    /// tracks long zero byte runs in progressive images
    pub eobrun: u16,

    /// if the previous value was also an eobrun then this is used to make sure
    /// that we don't have two non-maximum value runs in a row that we wouldn't be
    /// able to recode exactly the same way
    pub prev_eobrun: u16,
}

impl JpegPositionState {
    pub fn new(jf: &JpegHeader, mcu: u32) -> Self {
        let cmp = jf.cs_cmp[0];
        let mcumul = jf.cmp_info[cmp].sfv * jf.cmp_info[cmp].sfh;

        let state = JpegPositionState {
            cmp,
            mcu,
            csc: 0,
            sub: 0,
            dpos: mcu * mcumul,
            rstw: if jf.rsti != 0 {
                jf.rsti - (mcu % jf.rsti)
            } else {
                0
            },
            eobrun: 0,
            prev_eobrun: 0,
        };
        return state;
    }

    pub fn get_mcu(&self) -> u32 {
        self.mcu
    }
    pub fn get_dpos(&self) -> u32 {
        self.dpos
    }
    pub fn get_cmp(&self) -> usize {
        self.cmp
    }

    pub fn get_cumulative_reset_markers(&self, jf: &JpegHeader) -> u32 {
        if self.rstw != 0 {
            self.get_mcu() / jf.rsti
        } else {
            0
        }
    }

    pub fn reset_rstw(&mut self, jf: &JpegHeader) {
        self.rstw = jf.rsti;

        // eobruns don't span reset intervals
        self.prev_eobrun = 0;
    }

    /// calculates next position (non interleaved)
    fn next_mcu_pos_noninterleaved(&mut self, jf: &JpegHeader) -> JpegDecodeStatus {
        // increment position
        self.dpos += 1;

        let cmp_info = &jf.cmp_info[self.cmp];

        // fix for non interleaved mcu - horizontal
        if cmp_info.bch != cmp_info.nch && self.dpos % cmp_info.bch == cmp_info.nch {
            self.dpos += cmp_info.bch - cmp_info.nch;
        }

        // fix for non interleaved mcu - vertical
        if cmp_info.bcv != cmp_info.ncv && self.dpos / cmp_info.bch == cmp_info.ncv {
            self.dpos = cmp_info.bc;
        }

        // now we've updated dpos, update the current MCU to be a fraction of that
        if jf.jpeg_type == JpegType::Sequential {
            self.mcu = self.dpos / (cmp_info.sfv * cmp_info.sfh);
        }

        // check position
        if self.dpos >= cmp_info.bc {
            return JpegDecodeStatus::ScanCompleted;
        } else if jf.rsti > 0 {
            self.rstw -= 1;
            if self.rstw == 0 {
                return JpegDecodeStatus::RestartIntervalExpired;
            }
        }

        return JpegDecodeStatus::DecodeInProgress;
    }

    /// calculates next position for MCU
    pub fn next_mcu_pos(&mut self, jf: &JpegHeader) -> JpegDecodeStatus {
        // if there is just one component, go the simple route
        if jf.cs_cmpc == 1 {
            return self.next_mcu_pos_noninterleaved(jf);
        }

        let mut sta = JpegDecodeStatus::DecodeInProgress; // status
        let local_mcuh = jf.mcuh.get();
        let mut local_mcu = self.mcu;
        let mut local_cmp = self.cmp;

        // increment all counts where needed
        self.sub += 1;
        let mut local_sub = self.sub;
        if local_sub >= jf.cmp_info[local_cmp].mbs {
            self.sub = 0;
            local_sub = 0;

            self.csc += 1;

            if self.csc >= jf.cs_cmpc {
                self.csc = 0;
                self.cmp = jf.cs_cmp[0];
                local_cmp = self.cmp;

                self.mcu += 1;

                local_mcu = self.mcu;

                if local_mcu >= jf.mcuc {
                    sta = JpegDecodeStatus::ScanCompleted;
                } else if jf.rsti > 0 {
                    self.rstw -= 1;
                    if self.rstw == 0 {
                        sta = JpegDecodeStatus::RestartIntervalExpired;
                    }
                }
            } else {
                self.cmp = jf.cs_cmp[self.csc];
                local_cmp = self.cmp;
            }
        }

        let sfh = jf.cmp_info[local_cmp].sfh;
        let sfv = jf.cmp_info[local_cmp].sfv;

        // get correct position in image ( x & y )
        if sfh > 1 {
            // to fix mcu order
            let mcu_over_mcuh = local_mcu / local_mcuh;
            let sub_over_sfv = local_sub / sfv;
            let mcu_mod_mcuh = local_mcu - (mcu_over_mcuh * local_mcuh);
            let sub_mod_sfv = local_sub - (sub_over_sfv * sfv);
            let mut local_dpos = (mcu_over_mcuh * sfh) + sub_over_sfv;

            local_dpos *= jf.cmp_info[local_cmp].bch;
            local_dpos += (mcu_mod_mcuh * sfv) + sub_mod_sfv;

            self.dpos = local_dpos;
        } else if sfv > 1 {
            // simple calculation to speed up things if simple fixing is enough
            self.dpos = (local_mcu * jf.cmp_info[local_cmp].mbs) + local_sub;
        } else {
            // no calculations needed without subsampling
            self.dpos = self.mcu;
        }

        return sta;
    }
}
