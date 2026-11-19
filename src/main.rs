use g502_rs::Result;
use std::env;

fn main() -> Result<()> {
    let args: Vec<String> = env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("probe") => g502_rs::probe(),
        Some("battery") => g502_rs::device::battery::run(),
        Some("dpi") => match args.get(2).map(String::as_str) {
            Some("get") => g502_rs::device::dpi::read().map(|_| ()),
            Some("set") => {
                let dpi: u16 = args
                    .get(3)
                    .ok_or("Usage: dpi set <dpi> --level <1..5>")?
                    .parse()?;
                let pos = args
                    .iter()
                    .position(|a| a == "--level")
                    .ok_or("Required: --level <1..5>")?;
                let level: u8 = args.get(pos + 1).ok_or("Missing --level value")?.parse()?;
                g502_rs::device::dpi::write(dpi, level)
            }
            _ => Err("Usage: dpi get | dpi set <dpi> --level <1..5>".into()),
        },
        Some("decode") => {
            let file = args
                .get(2)
                .ok_or("Usage: cargo run -- decode <file.txt> [--known-g502x] [--rgb]")?;
            g502_rs::capture::decode(
                file,
                args.iter().any(|s| s == "--rgb"),
                args.iter().any(|s| s == "--known-g502x"),
            )
        }
        Some("experiment") => {
            let interface = args.get(2).ok_or("Usage: experiment <interface> <bus> <device> --label <text> --seconds <n> --output <file.json> [--baseline <file.json>] [--known-g502x]")?;
            let bus = args.get(3).ok_or("Missing USB bus number")?.parse()?;
            let address = args.get(4).ok_or("Missing USB device address")?.parse()?;
            let value = |name: &str| -> Option<&str> {
                args.windows(2)
                    .find(|p| p[0] == name)
                    .map(|p| p[1].as_str())
            };
            let label = value("--label").ok_or("Missing --label")?;
            let seconds = value("--seconds").ok_or("Missing --seconds")?.parse()?;
            let output = value("--output").ok_or("Missing --output")?;
            g502_rs::capture::experiment::record(g502_rs::capture::experiment::Options {
                interface,
                bus,
                address,
                label,
                seconds,
                output,
                baseline: value("--baseline"),
                known_g502x: args.iter().any(|s| s == "--known-g502x"),
            })
        }
        Some("live") => {
            let interface = args.get(2).ok_or(
                "Usage: cargo run -- live <tshark-interface-number> <bus> <device> [--rgb] [--all] [--log file.txt]"
            )?;
            let bus: u16 = args.get(3).ok_or("Missing USB bus number")?.parse()?;
            let address: u16 = args.get(4).ok_or("Missing USB device address")?.parse()?;
            let log_path = args
                .windows(2)
                .find(|pair| pair[0] == "--log")
                .map(|pair| pair[1].as_str());
            g502_rs::capture::live(
                interface,
                bus,
                address,
                args.iter().any(|s| s == "--rgb"),
                args.iter().any(|s| s == "--all"),
                log_path,
            )
        }
        _ => {
            println!(
                "Usage:\n  cargo run -- probe\n  cargo run -- battery\n  cargo run -- dpi get\n  cargo run -- dpi set 1600 --level 2\n  cargo run -- decode g502-requests.txt [--known-g502x] [--rgb]\n  cargo run -- experiment <interface> <bus> <device> --label <text> --seconds <n> --output <file.json> [--baseline <file.json>] [--known-g502x]\n  cargo run -- live <tshark-interface-number> <bus> <device> [--rgb] [--all] [--log file.txt]"
            );
            Ok(())
        }
    }
}
