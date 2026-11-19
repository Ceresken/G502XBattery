use super::protocol::{Match, Packet};
use hidapi::{HidApi, HidDevice};
use std::{
    error::Error,
    fmt,
    time::{Duration, Instant},
};

#[derive(Debug)]
pub enum RequestError {
    Timeout,
    Device(u8),
    Io(String),
    Invalid(&'static str),
}
impl fmt::Display for RequestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Timeout => write!(f, "HID++ request timed out"),
            Self::Device(e) => write!(f, "HID++ device error 0x{e:02X}"),
            Self::Io(e) => write!(f, "HID I/O error: {e}"),
            Self::Invalid(e) => write!(f, "{e}"),
        }
    }
}
impl Error for RequestError {}

/// An exchange backend; implementations must return within the supplied timeout.
pub trait Io {
    fn write(&mut self, packet: &[u8]) -> Result<(), RequestError>;
    fn read(&mut self, timeout: Duration) -> Result<Option<Vec<u8>>, RequestError>;
}
pub struct HidIo {
    short: HidDevice,
    long: HidDevice,
}
impl HidIo {
    pub fn open(vid: u16, pid: u16) -> crate::Result<Self> {
        let api = HidApi::new()?;
        let open = |usage| -> crate::Result<HidDevice> {
            let info = api
                .device_list()
                .find(|d| {
                    d.vendor_id() == vid
                        && d.product_id() == pid
                        && d.usage_page() == 0xff00
                        && d.usage() == usage
                })
                .ok_or_else(|| format!("Missing HID++ collection with usage 0x{usage:04X}"))?;
            Ok(api.open_path(info.path())?)
        };
        Ok(Self {
            short: open(1)?,
            long: open(2)?,
        })
    }
}
impl Io for HidIo {
    fn write(&mut self, packet: &[u8]) -> Result<(), RequestError> {
        let dev = if packet[0] == 0x10 {
            &self.short
        } else {
            &self.long
        };
        let n = dev
            .write(packet)
            .map_err(|e| RequestError::Io(e.to_string()))?;
        if n != packet.len() {
            return Err(RequestError::Io(format!(
                "Incomplete HID write: {n}/{} bytes",
                packet.len()
            )));
        }
        Ok(())
    }
    fn read(&mut self, timeout: Duration) -> Result<Option<Vec<u8>>, RequestError> {
        let deadline = Instant::now() + timeout;
        for dev in [&self.long, &self.short] {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            let ms = remaining.min(Duration::from_millis(25)).as_millis().max(1) as i32;
            let mut buf = [0; 64];
            let n = dev
                .read_timeout(&mut buf, ms)
                .map_err(|e| RequestError::Io(e.to_string()))?;
            if n > 0 {
                return Ok(Some(buf[..n].to_vec()));
            }
        }
        Ok(None)
    }
}
/// Exclusive mutable access serializes requests through this transport. Do not open
/// multiple transports to the same device concurrently; other processes may also interfere.
pub struct Transport<I> {
    io: I,
    slot: u8,
    next_id: u8,
    timeout: Duration,
}
impl<I: Io> Transport<I> {
    pub fn new(io: I, slot: u8) -> Self {
        Self {
            io,
            slot,
            next_id: 1,
            timeout: Duration::from_millis(2300),
        }
    }
    /// Retries are opt-in and should only be used for safe, read-only operations.
    pub fn request(
        &mut self,
        feature: u8,
        function: u8,
        params: &[u8],
        retries: u8,
    ) -> Result<Packet, RequestError> {
        for attempt in 0..=retries {
            let id = self.next_id;
            self.next_id = self.next_id % 15 + 1;
            let request = Packet::request(self.slot, feature, function, id, params)
                .map_err(RequestError::Invalid)?;
            println!("TX {request}");
            self.io.write(&request.encode())?;
            let deadline = Instant::now() + self.timeout;
            while Instant::now() < deadline {
                if let Some(data) = self
                    .io
                    .read(deadline.saturating_duration_since(Instant::now()))?
                    && let Some(packet) = Packet::decode(&data)
                {
                    match packet.classify(&request) {
                        Match::Response => {
                            println!("RX {packet}");
                            return Ok(packet);
                        }
                        Match::Error(code) => return Err(RequestError::Device(code)),
                        Match::Other => {}
                    }
                }
            }
            if attempt < retries {
                eprintln!(
                    "HID++ timeout, retrying read ({}/{retries})...",
                    attempt + 1
                );
            }
        }
        Err(RequestError::Timeout)
    }
}
