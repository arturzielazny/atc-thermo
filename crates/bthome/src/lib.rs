//! Decoder for BTHome v2 advertisement payloads.
//!
//! BTHome sensors put their readings in the service data of the 16-bit UUID
//! [`SERVICE_UUID16`]. The payload is one device-information byte followed by
//! a list of `(object id, value)` pairs, little-endian. See <https://bthome.io/format/>.
//!
//! # Examples
//!
//! ```
//! use bthome::{Object, Payload};
//!
//! let packet = bthome::parse(&[0x40, 0x00, 0xa4, 0x02, 0xc4, 0x08])?;
//! let Payload::Plain(objects) = packet.payload else { unreachable!() };
//! assert_eq!(objects, [Object::PacketId(0xa4), Object::Temperature(22.44)]);
//! # Ok::<(), bthome::ParseError>(())
//! ```

use std::error::Error;
use std::fmt;

/// 16-bit service UUID under which BTHome payloads are advertised.
pub const SERVICE_UUID16: u16 = 0xfcd2;

const FLAG_ENCRYPTED: u8 = 0x01;
const FLAG_TRIGGER_BASED: u8 = 0x04;
const VERSION_SHIFT: u8 = 5;
const SUPPORTED_VERSION: u8 = 2;

/// A decoded BTHome advertisement.
#[derive(Debug, Clone, PartialEq)]
pub struct Packet {
    /// The device sends only on events (button press, door open) rather than at a fixed interval.
    pub trigger_based: bool,
    /// The measurements, or the still-encrypted bytes.
    pub payload: Payload,
}

/// Packet contents.
#[derive(Debug, Clone, PartialEq)]
pub enum Payload {
    /// Decoded objects in the order they appear in the packet.
    Plain(Vec<Object>),
    /// AES-CCM encrypted objects; decrypting them needs the device's bind key.
    Encrypted(Vec<u8>),
}

/// One measurement from a packet. Values are in the units BTHome defines.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Object {
    /// Rolling counter; repeated adverts of one measurement share it.
    PacketId(u8),
    /// Battery level in %.
    Battery(u8),
    /// Temperature in °C.
    Temperature(f32),
    /// Relative humidity in %.
    Humidity(f32),
    /// Dew point in °C.
    DewPoint(f32),
    /// Pressure in hPa.
    Pressure(f32),
    /// Illuminance in lux.
    Illuminance(f32),
    /// Voltage in V.
    Voltage(f32),
    /// CO₂ concentration in ppm.
    Co2(u16),
    /// Binary sensor "power": `true` means on.
    Power(bool),
    /// Binary sensor "opening": `true` means open.
    Opening(bool),
    /// A valid object this decoder does not interpret, with its raw value bytes.
    Other { id: u8, raw: Vec<u8> },
}

/// Why a payload could not be decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ParseError {
    /// The payload has no device-information byte.
    Empty,
    /// The device-information byte announces a BTHome version other than 2.
    UnsupportedVersion(u8),
    /// An object id not in the BTHome v2 spec; its length is unknown, so decoding stops.
    UnknownObject { id: u8, offset: usize },
    /// The payload ends in the middle of an object.
    Truncated { id: u8, offset: usize },
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "empty payload"),
            Self::UnsupportedVersion(v) => write!(f, "unsupported BTHome version {v}"),
            Self::UnknownObject { id, offset } => {
                write!(f, "unknown object id {id:#04x} at offset {offset}")
            }
            Self::Truncated { id, offset } => {
                write!(f, "object {id:#04x} at offset {offset} is truncated")
            }
        }
    }
}

impl Error for ParseError {}

/// Decodes a BTHome v2 service-data payload.
///
/// # Errors
///
/// Returns a [`ParseError`] if the payload is empty, not BTHome v2, contains an
/// object id outside the spec, or ends in the middle of an object.
pub fn parse(data: &[u8]) -> Result<Packet, ParseError> {
    let (&info, body) = data.split_first().ok_or(ParseError::Empty)?;
    let version = info >> VERSION_SHIFT;
    if version != SUPPORTED_VERSION {
        return Err(ParseError::UnsupportedVersion(version));
    }
    let trigger_based = info & FLAG_TRIGGER_BASED != 0;
    if info & FLAG_ENCRYPTED != 0 {
        return Ok(Packet {
            trigger_based,
            payload: Payload::Encrypted(body.to_vec()),
        });
    }

    let mut objects = Vec::new();
    let mut offset = 1;
    while offset < data.len() {
        let id = data[offset];
        let start = offset + 1;
        let len = match value_len(id) {
            Some(Len::Fixed(n)) => n,
            Some(Len::Prefixed) => {
                let n = *data
                    .get(start)
                    .ok_or(ParseError::Truncated { id, offset })?;
                usize::from(n) + 1
            }
            None => return Err(ParseError::UnknownObject { id, offset }),
        };
        let raw = data
            .get(start..start + len)
            .ok_or(ParseError::Truncated { id, offset })?;
        objects.push(decode_object(id, raw));
        offset = start + len;
    }
    Ok(Packet {
        trigger_based,
        payload: Payload::Plain(objects),
    })
}

enum Len {
    Fixed(usize),
    /// First value byte is the length of the rest (text and raw objects).
    Prefixed,
}

/// Value length of each object id in the BTHome v2 spec.
fn value_len(id: u8) -> Option<Len> {
    let n = match id {
        0x00 | 0x01 | 0x09 | 0x0f..=0x11 | 0x15..=0x2f | 0x3a | 0x46 | 0x57..=0x59 | 0x60 => 1,
        0x02
        | 0x03
        | 0x06..=0x08
        | 0x0c..=0x0e
        | 0x12..=0x14
        | 0x3c
        | 0x3d
        | 0x3f..=0x41
        | 0x43..=0x45
        | 0x47..=0x4a
        | 0x51
        | 0x52
        | 0x56
        | 0x5a
        | 0x5d..=0x5f
        | 0xf0 => 2,
        0x04 | 0x05 | 0x0a | 0x0b | 0x42 | 0x4b | 0xf2 => 3,
        0x3e | 0x4c..=0x50 | 0x55 | 0x5b | 0x5c | 0xf1 => 4,
        0x53 | 0x54 => return Some(Len::Prefixed),
        _ => return None,
    };
    Some(Len::Fixed(n))
}

fn decode_object(id: u8, raw: &[u8]) -> Object {
    // Dividing (rather than multiplying by 0.01) rounds 2244 / 100 to exactly the f32 nearest 22.44.
    let unsigned_div = |divisor: f32| -> f32 { unsigned(raw) as f32 / divisor };
    let signed_div = |divisor: f32| -> f32 { signed(raw) as f32 / divisor };
    match id {
        0x00 => Object::PacketId(raw[0]),
        0x01 => Object::Battery(raw[0]),
        0x02 => Object::Temperature(signed_div(100.0)),
        0x45 => Object::Temperature(signed_div(10.0)),
        0x57 => Object::Temperature(signed_div(1.0)),
        0x58 => Object::Temperature(signed(raw) as f32 * 0.35),
        0x03 => Object::Humidity(unsigned_div(100.0)),
        0x2e => Object::Humidity(unsigned_div(1.0)),
        0x08 => Object::DewPoint(signed_div(100.0)),
        0x04 => Object::Pressure(unsigned_div(100.0)),
        0x05 => Object::Illuminance(unsigned_div(100.0)),
        0x0c => Object::Voltage(unsigned_div(1000.0)),
        0x4a => Object::Voltage(unsigned_div(10.0)),
        0x12 => Object::Co2(u16::from_le_bytes([raw[0], raw[1]])),
        0x10 => Object::Power(raw[0] != 0),
        0x11 => Object::Opening(raw[0] != 0),
        _ => Object::Other {
            id,
            raw: raw.to_vec(),
        },
    }
}

/// Little-endian unsigned integer of up to 4 bytes.
fn unsigned(raw: &[u8]) -> u32 {
    raw.iter()
        .rev()
        .fold(0, |acc, &b| (acc << 8) | u32::from(b))
}

/// Little-endian two's-complement integer of up to 4 bytes.
fn signed(raw: &[u8]) -> i32 {
    let bits = 32 - 8 * u32::try_from(raw.len()).expect("object values are at most 4 bytes");
    // Shift the value to the top of an i32, then arithmetic-shift back to sign-extend.
    i32::from_ne_bytes(unsigned(raw).to_ne_bytes()) << bits >> bits
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("valid hex in test"))
            .collect()
    }

    fn objects(s: &str) -> Vec<Object> {
        match parse(&hex(s)).expect("fixture parses").payload {
            Payload::Plain(objects) => objects,
            Payload::Encrypted(_) => panic!("fixture {s} is unexpectedly encrypted"),
        }
    }

    // Fixtures captured from LYWSD03MMC thermometers running pvvx firmware v5.9.
    #[test]
    fn pvvx_measurement_packet() {
        assert_eq!(
            objects("4000a4015702c408033e16"),
            [
                Object::PacketId(0xa4),
                Object::Battery(87),
                Object::Temperature(22.44),
                Object::Humidity(56.94),
            ]
        );
    }

    #[test]
    fn pvvx_voltage_and_switch_packet() {
        assert_eq!(
            objects("4000cc0c910b10001101"),
            [
                Object::PacketId(0xcc),
                Object::Voltage(2.961),
                Object::Power(false),
                Object::Opening(true),
            ]
        );
    }

    /// pvvx THB2 on a THB1, unencrypted (`bthome_beacon.c` `adv_set_data`): packet id,
    /// battery, temperature, humidity, voltage in one packet.
    #[test]
    fn thb1_measurement_packet() {
        assert_eq!(
            objects("40001c015702290903d4120cab0b"),
            [
                Object::PacketId(0x1c),
                Object::Battery(87),
                Object::Temperature(23.45),
                Object::Humidity(48.2),
                Object::Voltage(2.987),
            ]
        );
    }

    /// THB2 reed-switch event (`adv_set_event`, `SERVICE_RDS` on THB1): opened flag and
    /// a 32-bit counter, no temperature.
    #[test]
    fn thb1_event_packet() {
        assert_eq!(
            objects("40001d11013e2a000000"),
            [
                Object::PacketId(0x1d),
                Object::Opening(true),
                Object::Other {
                    id: 0x3e,
                    raw: vec![0x2a, 0, 0, 0]
                },
            ]
        );
    }

    #[test]
    fn negative_temperature() {
        // -5.25 °C = -525 = 0xfdf3
        assert_eq!(objects("4002f3fd"), [Object::Temperature(-5.25)]);
        assert_eq!(objects("4057fb"), [Object::Temperature(-5.0)]);
    }

    #[test]
    fn uninterpreted_objects_keep_their_bytes() {
        assert_eq!(
            objects("4042a08601"),
            [Object::Other {
                id: 0x42,
                raw: vec![0xa0, 0x86, 0x01]
            }]
        );
        assert_eq!(
            objects("405303616263"),
            [Object::Other {
                id: 0x53,
                raw: vec![3, b'a', b'b', b'c']
            }]
        );
    }

    #[test]
    fn trigger_flag() {
        let packet = parse(&hex("443a01")).expect("parses");
        assert!(packet.trigger_based);
    }

    #[test]
    fn encrypted_payload_is_returned_undecoded() {
        let packet = parse(&hex("41aabbcc")).expect("parses");
        assert_eq!(packet.payload, Payload::Encrypted(vec![0xaa, 0xbb, 0xcc]));
    }

    #[test]
    fn errors() {
        assert_eq!(parse(&[]), Err(ParseError::Empty));
        assert_eq!(parse(&hex("2000")), Err(ParseError::UnsupportedVersion(1)));
        assert_eq!(
            parse(&hex("4000017f01")),
            Err(ParseError::UnknownObject {
                id: 0x7f,
                offset: 3
            })
        );
        assert_eq!(
            parse(&hex("400001020a")),
            Err(ParseError::Truncated {
                id: 0x02,
                offset: 3
            })
        );
        assert_eq!(
            parse(&hex("4053")),
            Err(ParseError::Truncated {
                id: 0x53,
                offset: 1
            })
        );
    }
}
