use super::transport::{Io, RequestError, Transport};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Feature {
    pub index: u8,
    pub flags: u8,
    pub version: u8,
}
pub fn discover<I: Io>(
    transport: &mut Transport<I>,
    id: u16,
) -> Result<Option<Feature>, RequestError> {
    let [hi, lo] = id.to_be_bytes();
    let response = transport.request(0, 0, &[hi, lo, 0], 2)?;
    let p = response.params;
    Ok((p[0] != 0).then_some(Feature {
        index: p[0],
        flags: p[1],
        version: p[2],
    }))
}
pub fn require<I: Io>(transport: &mut Transport<I>, id: u16) -> crate::Result<Feature> {
    discover(transport, id)?.ok_or_else(|| format!("Feature 0x{id:04X} unsupported").into())
}
