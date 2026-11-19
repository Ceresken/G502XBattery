pub mod capture;
pub mod device;
pub mod hidpp;
use hidpp::{
    features,
    transport::{HidIo, RequestError, Transport},
};
use std::{collections::BTreeMap, fs::File, io::Write};
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
pub const SLOT: u8 = 1;
pub const MAP_PATH: &str = "feature-map.txt";
const FEATURES: &[(u16, &str)] = &[
    (0x1004, "Battery"),
    (0x2201, "Adjustable DPI"),
    (0x8081, "Per-zone lighting"),
    (0x8071, "RGB effects"),
    (0x8060, "Report rate"),
    (0x8100, "Onboard profiles"),
    (0x2121, "High-resolution wheel"),
    (0x8110, "Button reporting"),
];
pub fn open_mouse() -> Result<Transport<HidIo>> {
    Ok(Transport::new(HidIo::open(0x046d, 0xc547)?, SLOT))
}
pub fn probe() -> Result<()> {
    let mut transport = open_mouse()?;
    let mut found = BTreeMap::new();
    println!("Probing HID++ features on receiver 046D:C547, slot 1...");
    for &(id, name) in FEATURES {
        println!("0x{id:04X} {name}:");
        match features::discover(&mut transport, id) {
            Ok(Some(feature)) => {
                println!(
                    "  -> index 0x{:02X}, flags 0x{:02X}, v{}",
                    feature.index, feature.flags, feature.version
                );
                found.insert(id, feature.index);
            }
            Ok(None) => println!("  -> unsupported (index=0)"),
            Err(RequestError::Device(code)) => println!("  -> HID++ error 0x{code:02X}"),
            Err(RequestError::Timeout) => {
                println!("  -> TIMEOUT (not proof of unsupported feature)")
            }
            Err(e) => return Err(e.into()),
        }
    }
    if [0x1004, 0x2201, 0x8081]
        .iter()
        .all(|id| found.contains_key(id))
    {
        let mut file = File::create(MAP_PATH)?;
        for (id, index) in found {
            writeln!(file, "{id:04X} {index:02X}")?;
        }
        println!("Saved validated discovery results to {MAP_PATH}");
    } else {
        eprintln!("Incomplete discovery: NOT overwriting {MAP_PATH}.");
        eprintln!("To decode the recorded G502 X PLUS capture, use --known-g502x.");
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        device::{battery, dpi, polling},
        hidpp::{
            protocol::Packet,
            transport::{Io, RequestError},
        },
    };
    use std::{collections::VecDeque, time::Duration};
    struct Fake {
        replies: VecDeque<Vec<u8>>,
        writes: Vec<Vec<u8>>,
    }
    impl Io for Fake {
        fn write(&mut self, data: &[u8]) -> std::result::Result<(), RequestError> {
            self.writes.push(data.to_vec());
            Ok(())
        }
        fn read(&mut self, _: Duration) -> std::result::Result<Option<Vec<u8>>, RequestError> {
            Ok(self.replies.pop_front())
        }
    }
    fn fake(replies: Vec<Vec<u8>>) -> Transport<Fake> {
        Transport::new(
            Fake {
                replies: replies.into(),
                writes: vec![],
            },
            1,
        )
    }
    fn reply(feature: u8, command: u8, params: &[u8]) -> Vec<u8> {
        let mut data = vec![0x10, 1, feature, command, 0, 0, 0];
        data[4..4 + params.len()].copy_from_slice(params);
        data
    }
    #[test]
    fn battery_discovery_and_status() {
        let mut t = fake(vec![
            reply(0, 1, &[6, 0, 1]),
            reply(6, 2, &[0, 2, 0]),
            reply(6, 0x13, &[87, 3, 1]),
        ]);
        assert_eq!(
            battery::read(&mut t).unwrap(),
            battery::BatteryStatus {
                percentage: Some(87),
                level: 3,
                state: 1
            }
        );
    }
    #[test]
    fn dpi_reads_and_checks_set_readback() {
        let mut t = fake(vec![
            reply(0, 1, &[7, 0, 2]),
            reply(7, 0x22, &[0, 3, 0x20]),
            reply(7, 0x33, &[0, 0, 0]),
            reply(7, 0x24, &[0, 6, 0x40]),
        ]);
        assert_eq!(dpi::set(&mut t, 1600, 2).unwrap(), 1600);
        let mut t = fake(vec![
            reply(0, 1, &[7, 0, 2]),
            reply(7, 0x22, &[0, 3, 0x20]),
            reply(7, 0x33, &[0, 0, 0]),
            reply(7, 0x24, &[0, 3, 0x20]),
        ]);
        assert!(
            dpi::set(&mut t, 1600, 2)
                .unwrap_err()
                .to_string()
                .contains("mismatch")
        );
    }
    #[test]
    fn polling_discovery_and_verified_set() {
        let mut t = fake(vec![
            reply(0, 1, &[13, 0, 0]),
            reply(13, 2, &[0x8b, 0, 0]),
            reply(13, 0x13, &[1, 0, 0]),
            reply(13, 0x24, &[0, 0, 0]),
            reply(13, 5, &[0x8b, 0, 0]),
            reply(13, 0x16, &[2, 0, 0]),
        ]);
        let result = polling::set(&mut t, 2).unwrap();
        assert_eq!(result.intervals_ms, [1, 2, 4, 8]);
        assert_eq!(result.current_ms, 2);
        let mut t = fake(vec![
            reply(0, 1, &[13, 0, 0]),
            reply(13, 2, &[0x8b, 0, 0]),
            reply(13, 0x13, &[1, 0, 0]),
        ]);
        assert!(
            polling::set(&mut t, 3)
                .unwrap_err()
                .to_string()
                .contains("Unsupported")
        );
        let mut t = fake(vec![
            reply(0, 1, &[13, 0, 0]),
            reply(13, 2, &[0x8b, 0, 0]),
            reply(13, 0x13, &[1, 0, 0]),
            reply(13, 0x24, &[0, 0, 0]),
            reply(13, 5, &[0x8b, 0, 0]),
            reply(13, 0x16, &[1, 0, 0]),
        ]);
        assert!(
            polling::set(&mut t, 2)
                .unwrap_err()
                .to_string()
                .contains("mismatch")
        );
    }
    #[test]
    fn skips_unrelated_and_reports_device_errors() {
        let mut t = fake(vec![reply(9, 1, &[0, 0, 0]), reply(0, 1, &[7, 0, 2])]);
        assert_eq!(
            features::discover(&mut t, 0x2201).unwrap().unwrap().index,
            7
        );
        let mut t = fake(vec![vec![0x10, 1, 0xff, 0, 1, 5, 0]]);
        assert!(matches!(
            features::discover(&mut t, 0x2201),
            Err(RequestError::Device(5))
        ));
        assert!(Packet::decode(&[0x10, 1]).is_none());
    }
}
