use crate::{
    Result,
    hidpp::{
        features,
        transport::{Io, Transport},
    },
};
#[derive(Debug, PartialEq, Eq)]
pub struct BatteryStatus {
    pub percentage: Option<u8>,
    pub level: u8,
    pub state: u8,
}
pub fn read<I: Io>(transport: &mut Transport<I>) -> Result<BatteryStatus> {
    let idx = features::require(transport, 0x1004)?.index;
    let caps = transport.request(idx, 0, &[], 2)?;
    let info = transport.request(idx, 1, &[], 2)?;
    Ok(BatteryStatus {
        percentage: (caps.params[1] & 2 != 0).then_some(info.params[0]),
        level: info.params[1],
        state: info.params[2],
    })
}
pub fn run() -> Result<()> {
    let status = read(&mut crate::open_mouse()?)?;
    let state = match status.state {
        0 => "discharging",
        1 => "charging",
        2 => "charging (near full)",
        3 => "full",
        4 => "charging slowly",
        5 => "invalid battery",
        6 => "thermal error",
        7 => "charging error",
        _ => "unknown",
    };
    let percentage = status
        .percentage
        .map(|p| format!("{p}%"))
        .unwrap_or_else(|| "percentage gauge unavailable".into());
    println!(
        "Battery: {percentage} | level=0x{:02X} | {state}",
        status.level
    );
    Ok(())
}
