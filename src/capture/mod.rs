use crate::{MAP_PATH, Result, SLOT};
use std::{
    collections::{BTreeMap, HashMap},
    env,
    fs::File,
    io::{BufRead, BufReader, Write},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

fn known_g502x_map() -> HashMap<u16, u8> {
    HashMap::from([
        (0x1004, 0x06),
        (0x2201, 0x07),
        (0x2121, 0x08),
        (0x8071, 0x09),
        (0x8081, 0x0A),
        (0x8100, 0x0B),
        (0x8110, 0x0C),
        (0x8060, 0x0D),
    ])
}

fn load_map() -> Result<HashMap<u16, u8>> {
    let reader = BufReader::new(File::open(MAP_PATH)?);
    let mut map = HashMap::new();
    for line in reader.lines() {
        let line = line?;
        let parts: Vec<_> = line.split_whitespace().collect();
        if parts.len() != 2 {
            continue;
        }
        if let (Ok(id), Ok(index)) = (
            u16::from_str_radix(parts[0], 16),
            u8::from_str_radix(parts[1], 16),
        ) {
            map.insert(id, index);
        }
    }
    Ok(map)
}

// Accept HEX, colon-separated HEX, or TIME|HEX from tshark.
fn parse_line(line: &str) -> Option<(String, Vec<u8>)> {
    let (time, data) = line.split_once('|').unwrap_or(("", line));
    let hex: String = data.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if hex.len() != 14 && hex.len() != 40 {
        return None;
    }
    let bytes: Vec<u8> = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16))
        .collect::<std::result::Result<_, _>>()
        .ok()?;
    if !matches!(bytes[0], 0x10 | 0x11) {
        return None;
    }
    Some((time.trim().to_owned(), bytes))
}

pub fn decode(filename: &str, show_rgb: bool, known: bool) -> Result<()> {
    let mapping = if known {
        println!("Using known G502 X PLUS capture reference map (NOT live-verified).");
        known_g502x_map()
    } else {
        load_map().map_err(|_| "Cannot read feature-map.txt; run probe or add --known-g502x")?
    };
    let dpi_idx = mapping.get(&0x2201).copied();
    let rgb_idx = mapping.get(&0x8081).copied();
    println!("Feature indexes: DPI={dpi_idx:?}, RGB={rgb_idx:?}");
    if dpi_idx.is_none() {
        eprintln!("WARNING: no 0x2201 mapping; DPI cannot be labeled.");
    }

    let reader = BufReader::new(File::open(filename)?);
    let mut reports = 0usize;
    let mut dpi_requests = 0usize;
    let mut rgb_ranges = 0usize;
    let mut rgb_commits = 0usize;
    let mut rgb_printed = 0usize;
    let mut previous_dpi: Option<(u8, u16, Option<u8>)> = None;
    let mut groups: BTreeMap<(u8, u8), usize> = BTreeMap::new();

    for line in reader.lines() {
        let line = line?;
        let Some((time, data)) = parse_line(&line) else {
            continue;
        };
        if data.len() < 7 || data[1] != SLOT {
            continue;
        }
        reports += 1;
        let index = data[2];
        let function = data[3] >> 4;
        *groups.entry((index, function)).or_default() += 1;
        let stamp = if time.is_empty() { "?" } else { &time };

        if Some(index) == dpi_idx && function == 3 {
            dpi_requests += 1;
            let sensor = data[4];
            let dpi = u16::from_be_bytes([data[5], data[6]]);
            // On version 2 of 0x2201 there is an additional level field.
            let level = data.get(7).copied();
            let reading = (sensor, dpi, level);
            if previous_dpi != Some(reading) {
                println!(
                    "[{stamp:>12}] DPI set request: sensor={sensor}, dpi={dpi}, level={level:?}"
                );
                previous_dpi = Some(reading);
            }
        } else if Some(index) == rgb_idx && function == 5 {
            rgb_ranges += 1;
            if show_rgb && rgb_printed < 12 {
                for range in data[4..].as_chunks::<5>().0.iter().take(3) {
                    if range[0] == 0 {
                        continue;
                    }
                    println!(
                        "[{stamp:>12}] RGB zones {}..{} -> #{:02X}{:02X}{:02X}",
                        range[0], range[1], range[2], range[3], range[4]
                    );
                }
                rgb_printed += 1;
            }
        } else if Some(index) == rgb_idx && function == 7 {
            rgb_commits += 1;
        }
    }

    println!("\n--- SUMMARY ---");
    println!("HID++ reports: {reports}");
    println!("DPI set requests: {dpi_requests}");
    println!("RGB zone-range requests: {rgb_ranges}");
    println!("RGB frame commits: {rgb_commits}");
    println!("\nMost common feature/function pairs:");
    let mut ranking: Vec<_> = groups.into_iter().collect();
    ranking.sort_by_key(|a| std::cmp::Reverse(a.1));
    for ((index, function), count) in ranking.into_iter().take(12) {
        let id = mapping
            .iter()
            .find_map(|(&id, &idx)| (idx == index).then_some(id));
        let label = id
            .map(|id| format!("0x{id:04X}"))
            .unwrap_or_else(|| "unknown".to_owned());
        println!("  index 0x{index:02X}, {label}, fn {function:X}: {count} packets");
    }
    if show_rgb && rgb_ranges > rgb_printed {
        println!("RGB details capped at 12 packets; counts include everything.");
    }
    Ok(())
}

// Read-only USBPcap/TShark observer: this observes USB SET_REPORT payloads
// from ALL programs talking to a receiver. It does not attribute packets to G HUB.
// For live decoding, prefer the map written by `probe`. Known G502 X PLUS indexes
// are used as fallbacks if a probe timed out; never use this map for writes.
fn live_feature_map() -> HashMap<u16, u8> {
    let mut map = known_g502x_map();
    if let Ok(saved) = load_map() {
        map.extend(saved);
    }
    map
}

fn feature_name(feature_id: u16) -> &'static str {
    match feature_id {
        0x1004 => "Battery",
        0x2201 => "Adjustable DPI",
        0x8081 => "Per-zone lighting",
        0x8071 => "RGB effects",
        0x8060 => "Report rate",
        0x8100 => "Onboard profiles",
        0x2121 => "High-resolution wheel",
        0x8110 => "Button reporting",
        _ => "Other HID++ feature",
    }
}

fn hex_bytes(data: &[u8]) -> String {
    data.iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn emit_line(line: &str, log: &mut Option<File>) -> Result<()> {
    println!("{line}");
    if let Some(file) = log.as_mut() {
        writeln!(file, "{line}")?;
    }
    Ok(())
}

pub fn live(
    interface: &str,
    bus: u16,
    address: u16,
    show_rgb: bool,
    show_all: bool,
    log_path: Option<&str>,
) -> Result<()> {
    let mapping = live_feature_map();
    let mut reverse = HashMap::new();
    for (&id, &index) in &mapping {
        reverse.insert(index, id);
    }

    // -D on Windows usually lists USBPcap interfaces by numeric index.
    // Rust's Command handles spaces in C:\Program Files\... paths.
    let tshark = env::var("G502_TSHARK").unwrap_or_else(|_| {
        let installed = r"C:\Program Files\Wireshark\tshark.exe";
        if std::path::Path::new(installed).is_file() {
            installed.to_string()
        } else {
            "tshark".to_string()
        }
    });

    // Keep ONLY the selected receiver and USB data fragments.
    // Do not remove RGB at the capture stage: Rust counts/summarizes it.
    // This observes USB output control reports, not mouse input movement.
    let display_filter =
        format!("usb.bus_id == {bus} && usb.device_address == {address} && usb.data_fragment");
    let mut cmd = Command::new(&tshark);
    cmd.args([
        "-i",
        interface,
        "-l", // flush each packet immediately
        "-n", // no name resolution
        "-Y",
        &display_filter,
        "-T",
        "fields",
        "-E",
        "separator=|",
        "-E",
        "occurrence=f",
        "-e",
        "frame.time_relative",
        "-e",
        "usb.data_fragment",
    ]);
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::inherit());

    let mut log = if let Some(path) = log_path {
        Some(File::create(path)?)
    } else {
        None
    };

    emit_line(
        &format!("Listening on {interface}, USB bus {bus}, device {address}... (Ctrl+C to stop)"),
        &mut log,
    )?;
    emit_line(
        "Displaying outgoing HID++ commands. RGB animation traffic is summarized by default.",
        &mut log,
    )?;
    if !show_all {
        emit_line(
            "Repeats within 2 s are suppressed (except DPI). Use --all to disable.",
            &mut log,
        )?;
    }

    let mut child = cmd.spawn().map_err(|e| {
        format!("Unable to start TShark ({tshark}): {e}. Run 'tshark -D' or set G502_TSHARK.")
    })?;
    let stdout = child.stdout.take().ok_or("Failed to read TShark stdout")?;
    let reader = BufReader::new(stdout);

    let mut received = 0u64;
    let mut rgb_ranges = 0u64;
    let mut rgb_commits = 0u64;
    let mut repeated = 0u64;
    let mut last_printed: HashMap<Vec<u8>, Instant> = HashMap::new();
    let mut last_summary = Instant::now();

    for line in reader.lines() {
        let line = line?;
        let Some((stamp, data)) = parse_line(&line) else {
            continue;
        };
        if data.len() < 7 || data[1] != SLOT {
            continue;
        }
        received += 1;
        let index = data[2];
        let function = data[3] >> 4;
        let sw_id = data[3] & 0x0F;
        let id = reverse.get(&index).copied();
        let is_rgb_range = id == Some(0x8081) && function == 5;
        let is_rgb_pattern = id == Some(0x8081) && function == 3;
        let is_rgb_commit = id == Some(0x8081) && function == 7;

        if is_rgb_range {
            rgb_ranges += 1;
        }
        if is_rgb_commit {
            rgb_commits += 1;
        }

        if (is_rgb_range || is_rgb_commit || is_rgb_pattern) && !show_rgb {
            // These two calls typically make up nearly all G HUB RGB traffic.
        } else {
            let now = Instant::now();
            let is_dpi = id == Some(0x2201) && function == 3 && data.len() >= 7;
            let duplicate = !show_all
                && !is_dpi
                && last_printed
                    .get(&data)
                    .is_some_and(|prev| now.duration_since(*prev) < Duration::from_secs(2));
            if duplicate {
                repeated += 1;
            } else {
                last_printed.insert(data.clone(), now);
                let time = if stamp.is_empty() {
                    "?"
                } else {
                    stamp.as_str()
                };
                let detail = if is_dpi {
                    let dpi = u16::from_be_bytes([data[5], data[6]]);
                    let level = data
                        .get(7)
                        .map(|n| format!(", level={n}"))
                        .unwrap_or_default();
                    format!("DPI request: sensor={}, dpi={dpi}{level}", data[4])
                } else if is_rgb_range {
                    let zones = data[4..]
                        .as_chunks::<5>()
                        .0
                        .iter()
                        .take(3)
                        .filter(|c| c[0] != 0)
                        .map(|c| {
                            format!(
                                "zones {}-{} #{:02X}{:02X}{:02X}",
                                c[0], c[1], c[2], c[3], c[4]
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!("RGB range request: {zones}")
                } else if is_rgb_commit {
                    "RGB frame commit request".to_owned()
                } else if let Some(feature_id) = id {
                    format!(
                        "{} (0x{feature_id:04X}) fn={function:X} sw={sw_id:X} params={}",
                        feature_name(feature_id),
                        hex_bytes(&data[4..])
                    )
                } else {
                    format!(
                        "Unknown feature index=0x{index:02X} fn={function:X} sw={sw_id:X} params={}",
                        hex_bytes(&data[4..])
                    )
                };
                let raw = if show_all {
                    format!(" | raw={}", hex_bytes(&data))
                } else {
                    String::new()
                };
                emit_line(&format!("[{time:>11}] TX {detail}{raw}"), &mut log)?;
            }
        }

        // A single statistics line replaces hundreds of RGB animation reports.
        if last_summary.elapsed() >= Duration::from_secs(5) {
            emit_line(
                &format!(
                    "[stats] {received} reports | RGB ranges {rgb_ranges}, commits {rgb_commits} | duplicates hidden {repeated}"
                ),
                &mut log,
            )?;
            last_summary = Instant::now();
            // Keep memory stable for long capture sessions.
            last_printed.retain(|_, seen| seen.elapsed() < Duration::from_secs(3));
        }
    }

    let status = child.wait()?;
    emit_line(
        &format!(
            "Stopped ({status}). Processed {received} reports, including {rgb_ranges} RGB range and {rgb_commits} RGB frame-commit requests."
        ),
        &mut log,
    )?;
    if !status.success() {
        return Err(
            format!("TShark exited unsuccessfully ({status}); check its messages above.").into(),
        );
    }
    Ok(())
}

pub mod experiment;
