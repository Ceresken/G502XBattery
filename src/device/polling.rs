//! HID++ 0x8060, based on libratbag's adjustable report-rate implementation:
//! https://github.com/libratbag/libratbag/blob/master/src/hidpp20.c
use crate::{
    Result,
    hidpp::{
        features,
        transport::{Io, Transport},
    },
};
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Polling {
    pub intervals_ms: Vec<u8>,
    pub current_ms: u8,
}
fn read_at<I: Io>(t: &mut Transport<I>, index: u8) -> Result<Polling> {
    let bitmap = t.request(index, 0, &[], 2)?.params[0];
    let current_ms = t.request(index, 1, &[], 2)?.params[0];
    Ok(Polling {
        intervals_ms: (1..=8).filter(|ms| bitmap & (1 << (ms - 1)) != 0).collect(),
        current_ms,
    })
}
pub fn get<I: Io>(t: &mut Transport<I>) -> Result<Polling> {
    let idx = features::require(t, 0x8060)?.index;
    read_at(t, idx)
}
pub fn set<I: Io>(t: &mut Transport<I>, ms: u8) -> Result<Polling> {
    let idx = features::require(t, 0x8060)?.index;
    let before = read_at(t, idx)?;
    if !before.intervals_ms.contains(&ms) {
        return Err(format!("Unsupported polling interval: {ms} ms").into());
    }
    t.request(idx, 2, &[ms], 0)?;
    let after = read_at(t, idx)?;
    if after.current_ms != ms {
        return Err(format!(
            "Polling readback mismatch: requested {ms} ms, received {} ms",
            after.current_ms
        )
        .into());
    }
    Ok(after)
}
