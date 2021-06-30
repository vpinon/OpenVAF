/*
 *  ******************************************************************************************
 *  Copyright (c) 2021 Pascal Kuthe. This file is part of the frontend project.
 *  It is subject to the license terms in the LICENSE file found in the top-level directory
 *  of this distribution and at  https://gitlab.com/DSPOM/OpenVAF/blob/master/LICENSE.
 *  No part of frontend, including this file, may be copied, modified, propagated, or
 *  distributed except according to the terms contained in the LICENSE file.
 *  *****************************************************************************************
 */

pub use string::OsdiStr;

pub use osdi_types as types;

pub mod model_info_store;

mod string;

mod ids;
mod serialization;

pub use crate::model_info_store::ModelInfoStore;
use bitflags::bitflags;

bitflags! {
    pub struct ReturnFlags: u64{
        const EMPTY = 0;
        const FINISH_ON_SUCCESS = 1;
        const ABORT = 2;
    }
}

bitflags! {
    pub struct LoadFlags: u32{
        const EMPTY = 0;
        const AC = 1;
    }
}

#[cfg(feature = "simulator")]
pub mod runtime;

#[cfg(feature = "simulator")]
pub use runtime::{abi, OsdiModel};
