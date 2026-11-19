use crate::{
    Result,
    hidpp::{
        features,
        transport::{Io, Transport},
    },
};
fn get_at<I: Io>(transport: &mut Transport<I>, idx: u8) -> Result<u16> {
    let response = transport.request(idx, 2, &[0, 0, 0], 2)?;
    if response.params[0] != 0 {
        return Err("DPI sensor index in reply is not 0".into());
    }
    Ok(u16::from_be_bytes([response.params[1], response.params[2]]))
}
pub fn get<I: Io>(transport: &mut Transport<I>) -> Result<u16> {
    let idx = features::require(transport, 0x2201)?.index;
    get_at(transport, idx)
}
pub fn set<I: Io>(transport: &mut Transport<I>, dpi: u16, level: u8) -> Result<u16> {
    if !(100..=25600).contains(&dpi) {
        return Err("DPI must be between 100 and 25600".into());
    }
    if !(1..=5).contains(&level) {
        return Err("The observed level byte was 1..=5; pass --level 1..5".into());
    }
    let idx = features::require(transport, 0x2201)?.index;
    println!("Before: {} DPI", get_at(transport, idx)?);
    let [hi, lo] = dpi.to_be_bytes();
    // The fourth byte was observed in captured traffic; its meaning is not established.
    transport.request(idx, 3, &[0, hi, lo, level], 0)?;
    let observed = get_at(transport, idx)?;
    println!("After: {observed} DPI");
    if observed != dpi {
        return Err(format!("DPI readback mismatch: requested {dpi}, received {observed}. G HUB may be overwriting the setting.").into());
    }
    Ok(observed)
}
pub fn read() -> Result<u16> {
    let dpi = get(&mut crate::open_mouse()?)?;
    println!("Current DPI: {dpi}");
    Ok(dpi)
}
pub fn write(dpi: u16, level: u8) -> Result<()> {
    set(&mut crate::open_mouse()?, dpi, level)?;
    println!("DPI readback verified. (This is not proof of permanent onboard storage.)");
    Ok(())
}
