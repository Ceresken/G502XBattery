use crate::{
    Result,
    hidpp::protocol::{Match, Packet},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{BufRead, BufReader, BufWriter, Write},
    process::{Command, Stdio},
};

const SCHEMA: u32 = 1;
const MATCH_WINDOW_SECONDS: f64 = 2.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Outgoing,
    Incoming,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub frame: u64,
    pub timestamp: String,
    pub relative_seconds: f64,
    pub direction: Direction,
    pub raw: Vec<u8>,
    pub slot: u8,
    pub feature_index: u8,
    pub function: u8,
    pub software_id: u8,
    /// Frame number of a possible matching request, not proof that this is a response.
    pub request_frame: Option<u64>,
    pub correlation: String,
}
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Key {
    pub slot: u8,
    pub feature_index: u8,
    pub function: u8,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Group {
    pub key: Key,
    pub count: u64,
    pub per_second: f64,
    pub parameter_variants: Vec<String>,
    pub recurring: bool,
    pub possible_rgb_animation: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Change {
    pub key: Key,
    pub baseline_count: u64,
    pub experiment_count: u64,
    pub baseline_per_second: f64,
    pub experiment_per_second: f64,
    pub new_parameters: Vec<String>,
    pub missing_parameters: Vec<String>,
    pub present_in_baseline: bool,
    pub possible_rgb_animation: bool,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Experiment {
    pub schema_version: u32,
    pub label: String,
    pub duration_seconds: f64,
    pub bus: u16,
    pub address: u16,
    pub baseline_label: Option<String>,
    pub feature_map: BTreeMap<u16, u8>,
    pub groups: Vec<Group>,
    pub changes: Vec<Change>,
    pub packets: Vec<Event>,
}

fn hex(data: &[u8]) -> String {
    data.iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}
fn bytes(s: &str) -> Option<Vec<u8>> {
    let s = s.trim().replace([':', ' '], "");
    if s.len() != 14 && s.len() != 40 {
        return None;
    }
    let b = s.as_bytes();
    if !b.iter().all(u8::is_ascii_hexdigit) {
        return None;
    }
    b.as_chunks::<2>()
        .0
        .iter()
        .map(|p| u8::from_str_radix(std::str::from_utf8(p).ok()?, 16).ok())
        .collect()
}
// tshark fields: frame, epoch, relative, source, destination, endpoint direction,
// URB type, data_fragment, HID data, capture data, control response.
fn parse_tshark(line: &str) -> Option<Event> {
    let fields: Vec<&str> = line.split('|').collect();
    if fields.len() != 11 {
        return None;
    }
    let direction = if fields[3] == "host" {
        Direction::Outgoing
    } else if fields[4] == "host" {
        Direction::Incoming
    } else {
        return None;
    };
    // Source/destination represent the actual transfer stage. Reject contradictory
    // URB phases to avoid counting the submit and completion of a transfer twice.
    let urb = fields[6];
    if direction == Direction::Outgoing
        && !(urb.is_empty() || urb == "S" || urb == "0x53" || urb == "83")
    {
        return None;
    }
    if direction == Direction::Incoming
        && !(urb.is_empty() || urb == "C" || urb == "0x43" || urb == "67")
    {
        return None;
    }
    let raw = fields[7..]
        .iter()
        .find_map(|s| bytes(s).filter(|b| Packet::decode(b).is_some()))?;
    let p = Packet::decode(&raw)?;
    let relative_seconds: f64 = fields[2].parse().ok()?;
    if !relative_seconds.is_finite() || relative_seconds < 0.0 {
        return None;
    }
    Some(Event {
        frame: fields[0].parse().ok()?,
        timestamp: fields[1].to_owned(),
        relative_seconds,
        direction,
        raw,
        slot: p.slot,
        feature_index: p.feature,
        function: p.command >> 4,
        software_id: p.command & 15,
        request_frame: None,
        correlation: if direction == Direction::Outgoing {
            "request_unmatched"
        } else {
            "incoming_unmatched"
        }
        .into(),
    })
}

fn correlate(events: &mut [Event]) {
    let mut pending: Vec<usize> = Vec::new();
    for i in 0..events.len() {
        let now = events[i].relative_seconds;
        pending.retain(|&p| {
            now >= events[p].relative_seconds
                && now - events[p].relative_seconds <= MATCH_WINDOW_SECONDS
        });
        if events[i].direction == Direction::Outgoing {
            pending.push(i);
            continue;
        }
        let Some(response) = Packet::decode(&events[i].raw) else {
            continue;
        };
        let candidates: Vec<(usize, bool)> = pending
            .iter()
            .enumerate()
            .filter_map(|(pos, &p)| {
                let req = Packet::decode(&events[p].raw)?;
                match response.classify(&req) {
                    Match::Response => Some((pos, false)),
                    Match::Error(_) => Some((pos, true)),
                    Match::Other => None,
                }
            })
            .collect();
        if candidates.len() == 1 {
            let (pos, error) = candidates[0];
            let p = pending.remove(pos);
            events[i].request_frame = Some(events[p].frame);
            events[i].correlation = if error {
                "possible_error_response"
            } else {
                "possible_response"
            }
            .into();
            events[p].correlation = "possible_response_found".into();
        } else if candidates.len() > 1 {
            events[i].correlation = "ambiguous_multiple_requests".into();
        }
    }
}
fn groups(events: &[Event], duration: f64, rgb: Option<u8>) -> Vec<Group> {
    let mut counts: BTreeMap<Key, (u64, BTreeSet<String>)> = BTreeMap::new();
    for e in events.iter().filter(|e| e.direction == Direction::Outgoing) {
        let key = Key {
            slot: e.slot,
            feature_index: e.feature_index,
            function: e.function,
        };
        let entry = counts.entry(key).or_default();
        entry.0 += 1;
        entry.1.insert(hex(&e.raw[4..]));
    }
    counts
        .into_iter()
        .map(|(key, (count, variants))| {
            let possible_rgb_animation =
                rgb == Some(key.feature_index) && matches!(key.function, 3 | 5 | 7);
            Group {
                key,
                count,
                per_second: count as f64 / duration,
                parameter_variants: variants.into_iter().collect(),
                recurring: count >= 3,
                possible_rgb_animation,
            }
        })
        .collect()
}
fn compare(current: &[Group], baseline: &[Group]) -> Vec<Change> {
    let a: BTreeMap<_, _> = baseline.iter().map(|g| (&g.key, g)).collect();
    let b: BTreeMap<_, _> = current.iter().map(|g| (&g.key, g)).collect();
    let keys: BTreeSet<_> = a.keys().chain(b.keys()).copied().collect();
    keys.into_iter()
        .map(|key| {
            let old = a.get(key).copied();
            let new = b.get(key).copied();
            let old_params: BTreeSet<_> = old
                .into_iter()
                .flat_map(|g| &g.parameter_variants)
                .cloned()
                .collect();
            let new_params: BTreeSet<_> = new
                .into_iter()
                .flat_map(|g| &g.parameter_variants)
                .cloned()
                .collect();
            Change {
                key: key.clone(),
                baseline_count: old.map_or(0, |g| g.count),
                experiment_count: new.map_or(0, |g| g.count),
                baseline_per_second: old.map_or(0.0, |g| g.per_second),
                experiment_per_second: new.map_or(0.0, |g| g.per_second),
                new_parameters: new_params.difference(&old_params).cloned().collect(),
                missing_parameters: old_params.difference(&new_params).cloned().collect(),
                present_in_baseline: old.is_some(),
                possible_rgb_animation: old.or(new).is_some_and(|g| g.possible_rgb_animation),
            }
        })
        .collect()
}

fn preview(values: &[String]) -> String {
    let mut result = values
        .iter()
        .take(5)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    if values.len() > 5 {
        result.push_str(&format!(" ... ({} total)", values.len()));
    }
    result
}

pub struct Options<'a> {
    pub interface: &'a str,
    pub bus: u16,
    pub address: u16,
    pub label: &'a str,
    pub seconds: u64,
    pub output: &'a str,
    pub baseline: Option<&'a str>,
    pub known_g502x: bool,
}
pub fn record(options: Options<'_>) -> Result<()> {
    if options.seconds == 0 || options.label.trim().is_empty() {
        return Err("A nonempty label and positive duration are required".into());
    }
    if options
        .baseline
        .is_some_and(|p| std::path::Path::new(p) == std::path::Path::new(options.output))
    {
        return Err("Output must differ from baseline".into());
    }
    let baseline: Option<Experiment> = options
        .baseline
        .map(|p| -> Result<Experiment> { Ok(serde_json::from_reader(File::open(p)?)?) })
        .transpose()?;
    if let Some(b) = &baseline {
        if b.schema_version != SCHEMA {
            return Err("Unsupported baseline schema version".into());
        }
        if b.bus != options.bus || b.address != options.address {
            eprintln!(
                "Warning: baseline USB bus/address differs; ensure this is the same device and capture context."
            );
        }
    }
    let mut map: BTreeMap<_, _> = if options.known_g502x {
        super::known_g502x_map().into_iter().collect()
    } else {
        BTreeMap::new()
    };
    if let Ok(saved) = super::load_map() {
        map.extend(saved);
    }
    if let Some(b) = &baseline
        && b.feature_map != map
    {
        eprintln!(
            "Warning: feature maps differ; feature indexes may have changed, so comparisons may be misleading."
        );
    }
    let tshark = std::env::var("G502_TSHARK").unwrap_or_else(|_| {
        let p = r"C:\Program Files\Wireshark\tshark.exe";
        if std::path::Path::new(p).is_file() {
            p.into()
        } else {
            "tshark".into()
        }
    });
    let filter = format!(
        "usb.bus_id == {} && usb.device_address == {}",
        options.bus, options.address
    );
    let seconds = format!("duration:{}", options.seconds);
    let mut cmd = Command::new(&tshark);
    cmd.args([
        "-i",
        options.interface,
        "-a",
        &seconds,
        "-l",
        "-n",
        "-Y",
        &filter,
        "-T",
        "fields",
        "-E",
        "separator=|",
        "-E",
        "occurrence=f",
    ]);
    for field in [
        "frame.number",
        "frame.time_epoch",
        "frame.time_relative",
        "usb.src",
        "usb.dst",
        "usb.endpoint_address.direction",
        "usb.urb_type",
        "usb.data_fragment",
        "usbhid.data",
        "usb.capdata",
        "usb.control.Response",
    ] {
        cmd.args(["-e", field]);
    }
    cmd.stdout(Stdio::piped()).stderr(Stdio::inherit());
    println!(
        "Recording '{}' for {} seconds on bus {}, device {}...",
        options.label, options.seconds, options.bus, options.address
    );
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Unable to start TShark ({tshark}): {e}"))?;
    let stdout = child.stdout.take().ok_or("TShark stdout unavailable")?;
    let mut packets = Vec::new();
    for line in BufReader::new(stdout).lines() {
        if let Some(event) = parse_tshark(&line?) {
            packets.push(event);
        }
    }
    let status = child.wait()?;
    if !status.success() {
        return Err(format!("TShark failed ({status}); no experiment file written").into());
    }
    correlate(&mut packets);
    let current = groups(&packets, options.seconds as f64, map.get(&0x8081).copied());
    let changes = baseline
        .as_ref()
        .map_or_else(Vec::new, |b| compare(&current, &b.groups));
    let experiment = Experiment {
        schema_version: SCHEMA,
        label: options.label.into(),
        duration_seconds: options.seconds as f64,
        bus: options.bus,
        address: options.address,
        baseline_label: baseline.map(|b| b.label),
        feature_map: map,
        groups: current,
        changes,
        packets,
    };
    println!(
        "Captured {} HID++ packets ({} possible responses).",
        experiment.packets.len(),
        experiment
            .packets
            .iter()
            .filter(|p| p.request_frame.is_some())
            .count()
    );
    if experiment.packets.is_empty() {
        eprintln!(
            "Warning: no HID++ packets captured; check interface, receiver address and TShark dissectors."
        );
    }
    if options.baseline.is_some() {
        for c in &experiment.changes {
            if !c.present_in_baseline
                || !c.new_parameters.is_empty()
                || !c.missing_parameters.is_empty()
                || (c.experiment_per_second - c.baseline_per_second).abs() >= 1.0
            {
                println!(
                    "slot {} index 0x{:02X} fn {:X}: {:.2}/s -> {:.2}/s; new [{}]; missing [{}]{}",
                    c.key.slot,
                    c.key.feature_index,
                    c.key.function,
                    c.baseline_per_second,
                    c.experiment_per_second,
                    preview(&c.new_parameters),
                    preview(&c.missing_parameters),
                    if c.possible_rgb_animation {
                        " (possible RGB animation)"
                    } else {
                        ""
                    }
                );
            }
        }
    } else {
        for g in &experiment.groups {
            if g.recurring {
                println!(
                    "Recurring: slot {} index 0x{:02X} fn {:X}, {} ({:.2}/s){}",
                    g.key.slot,
                    g.key.feature_index,
                    g.key.function,
                    g.count,
                    g.per_second,
                    if g.possible_rgb_animation {
                        " (possible RGB animation)"
                    } else {
                        ""
                    }
                );
            }
        }
    }
    let mut output = BufWriter::new(File::create(options.output)?);
    serde_json::to_writer_pretty(&mut output, &experiment)?;
    output.flush()?;
    println!("Saved {}", options.output);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn event(frame: u64, time: f64, direction: Direction, raw: &[u8]) -> Event {
        let p = Packet::decode(raw).unwrap();
        Event {
            frame,
            timestamp: format!("{time}"),
            relative_seconds: time,
            direction,
            raw: raw.to_vec(),
            slot: p.slot,
            feature_index: p.feature,
            function: p.command >> 4,
            software_id: p.command & 15,
            request_frame: None,
            correlation: String::new(),
        }
    }
    #[test]
    fn parses_direction_and_transfer_stage() {
        let out = "1|1700000000.123|0.123|host|1.2.0|0|S|10:01:0d:12:aa:00:00|||";
        let input = "2|1700000000.124|0.124|1.2.1|host|1||||10010d12bb0000|";
        assert_eq!(parse_tshark(out).unwrap().direction, Direction::Outgoing);
        assert_eq!(parse_tshark(input).unwrap().direction, Direction::Incoming);
        assert_eq!(
            parse_tshark("1|1700000000.000000000|0.000000000|host|1.2.0|0||||10010d12010000|")
                .unwrap()
                .direction,
            Direction::Outgoing
        );
        assert!(parse_tshark(&out.replace("|S|", "|C|")).is_none());
        assert!(parse_tshark(&out.replace("|host|", "|unknown|")).is_none());
    }
    #[test]
    fn correlation_exact_error_and_ambiguous() {
        let request = [0x10, 1, 0x0d, 0x12, 0, 0, 0];
        let mut events = vec![
            event(1, 0.0, Direction::Outgoing, &request),
            event(2, 0.1, Direction::Incoming, &[0x10, 1, 0x0d, 0x13, 0, 0, 0]),
            event(
                3,
                0.2,
                Direction::Incoming,
                &[0x10, 1, 0xff, 0x0d, 0x12, 5, 0],
            ),
        ];
        correlate(&mut events);
        assert_eq!(events[1].request_frame, None);
        assert_eq!(events[2].request_frame, Some(1));
        assert_eq!(events[2].correlation, "possible_error_response");
        let mut events = vec![
            event(1, 0.0, Direction::Outgoing, &request),
            event(2, 0.1, Direction::Outgoing, &request),
            event(3, 0.2, Direction::Incoming, &request),
        ];
        correlate(&mut events);
        assert_eq!(events[2].correlation, "ambiguous_multiple_requests");
        assert_eq!(events[2].request_frame, None);
        let mut events = vec![
            event(1, 0.0, Direction::Outgoing, &request),
            event(2, 3.0, Direction::Incoming, &request),
        ];
        correlate(&mut events);
        assert_eq!(events[1].request_frame, None);
    }
    #[test]
    fn baseline_detects_parameter_changes_and_normalizes_rate() {
        let a = [0x10, 1, 13, 0x12, 1, 0, 0];
        let b = [0x10, 1, 13, 0x12, 2, 0, 0];
        let old = groups(&[event(1, 0.0, Direction::Outgoing, &a)], 1.0, None);
        let new = groups(
            &[
                event(2, 0.0, Direction::Outgoing, &b),
                event(3, 0.1, Direction::Outgoing, &b),
            ],
            2.0,
            None,
        );
        let c = compare(&new, &old);
        assert_eq!(c[0].new_parameters, ["02 00 00"]);
        assert_eq!(c[0].missing_parameters, ["01 00 00"]);
        assert_eq!(c[0].baseline_per_second, c[0].experiment_per_second);
    }
    #[test]
    fn serialization_preserves_raw_and_label() {
        let g = groups(
            &[event(
                1,
                0.0,
                Direction::Outgoing,
                &[0x10, 1, 2, 3, 4, 5, 6],
            )],
            1.0,
            None,
        );
        let e = Experiment {
            schema_version: 1,
            label: "Polling 1000 ? 500 Hz".into(),
            duration_seconds: 1.0,
            bus: 1,
            address: 2,
            baseline_label: None,
            feature_map: BTreeMap::new(),
            groups: g,
            changes: vec![],
            packets: vec![event(
                1,
                0.0,
                Direction::Outgoing,
                &[0x10, 1, 2, 3, 4, 5, 6],
            )],
        };
        let s = serde_json::to_string(&e).unwrap();
        let decoded: Experiment = serde_json::from_str(&s).unwrap();
        assert_eq!(decoded.label, e.label);
        assert_eq!(decoded.packets[0].raw, e.packets[0].raw);
    }
}
