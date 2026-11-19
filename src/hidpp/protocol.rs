use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packet {
    pub report: u8,
    pub slot: u8,
    pub feature: u8,
    pub command: u8,
    pub params: Vec<u8>,
}

impl Packet {
    pub fn request(
        slot: u8,
        feature: u8,
        function: u8,
        software_id: u8,
        params: &[u8],
    ) -> Result<Self, &'static str> {
        if function > 15 || !(1..=15).contains(&software_id) || params.len() > 16 {
            return Err("Invalid HID++ request parameters");
        }
        let report = if params.len() <= 3 { 0x10 } else { 0x11 };
        let mut padded = vec![0; if report == 0x10 { 3 } else { 16 }];
        padded[..params.len()].copy_from_slice(params);
        Ok(Self {
            report,
            slot,
            feature,
            command: (function << 4) | software_id,
            params: padded,
        })
    }
    pub fn encode(&self) -> Vec<u8> {
        let mut out = vec![self.report, self.slot, self.feature, self.command];
        out.extend_from_slice(&self.params);
        out
    }
    pub fn decode(data: &[u8]) -> Option<Self> {
        let len = match *data.first()? {
            0x10 => 7,
            0x11 => 20,
            _ => return None,
        };
        if data.len() < len {
            return None;
        }
        Some(Self {
            report: data[0],
            slot: data[1],
            feature: data[2],
            command: data[3],
            params: data[4..len].to_vec(),
        })
    }
    pub fn classify(&self, request: &Self) -> Match {
        if self.slot != request.slot {
            return Match::Other;
        }
        if matches!(self.feature, 0xff | 0x8f)
            && self.command == request.feature
            && self.params.first() == Some(&request.command)
        {
            return self
                .params
                .get(1)
                .copied()
                .map_or(Match::Other, Match::Error);
        }
        if self.feature == request.feature && self.command == request.command {
            Match::Response
        } else {
            Match::Other
        }
    }
}
#[derive(Debug, PartialEq, Eq)]
pub enum Match {
    Response,
    Error(u8),
    Other,
}
impl fmt::Display for Packet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, b) in self.encode().iter().enumerate() {
            if i > 0 {
                write!(f, " ")?;
            }
            write!(f, "{b:02X}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn encoding_and_validation() {
        let p = Packet::request(1, 2, 3, 4, &[5, 6, 7]).unwrap();
        assert_eq!(p.encode(), [0x10, 1, 2, 0x34, 5, 6, 7]);
        assert_eq!(Packet::decode(&p.encode()), Some(p));
        assert_eq!(
            Packet::request(1, 2, 3, 4, &[1, 2, 3, 4])
                .unwrap()
                .encode()
                .len(),
            20
        );
        assert!(Packet::request(1, 2, 16, 4, &[]).is_err());
        assert!(Packet::decode(&[0x11; 7]).is_none());
    }
    #[test]
    fn matches_exactly_and_errors() {
        let p = Packet::request(1, 2, 3, 4, &[]).unwrap();
        assert_eq!(p.classify(&p), Match::Response);
        let mut other = p.clone();
        other.command ^= 1;
        assert_eq!(other.classify(&p), Match::Other);
        other = p.clone();
        other.slot = 2;
        assert_eq!(other.classify(&p), Match::Other);
        let err = Packet::decode(&[0x10, 1, 0xff, 2, 0x34, 7, 0]).unwrap();
        assert_eq!(err.classify(&p), Match::Error(7));
    }
}
