//! Latest readings and history of every BTHome device heard so far.

use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, SystemTime};

use bluer::Address;
use bthome::{Object, ParseError, Payload};

use crate::scanner::Advert;

/// How far back history is kept in memory; matches the longest chart window.
pub const HISTORY_SPAN: Duration = Duration::from_secs(24 * 60 * 60);

/// One temperature and humidity measurement.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    pub at: SystemTime,
    pub temperature: f32,
    pub humidity: f32,
}

/// A sample together with the device context it was taken in; the unit of persistence.
#[derive(Debug, Clone, PartialEq)]
pub struct Measurement {
    pub address: Address,
    pub name: String,
    pub sample: Sample,
    pub battery: Option<u8>,
    pub voltage: Option<f32>,
}

/// A thermometer and its most recent values.
#[derive(Debug)]
pub struct Device {
    pub address: Address,
    pub name: String,
    pub rssi: Option<i16>,
    pub temperature: Option<f32>,
    pub humidity: Option<f32>,
    pub battery: Option<u8>,
    pub voltage: Option<f32>,
    pub last_seen: SystemTime,
    /// The device sends encrypted adverts, which need a bind key to read.
    pub encrypted: bool,
    last_sample_packet: Option<u8>,
    history: VecDeque<Sample>,
}

impl Device {
    fn new(address: Address, name: String, seen: SystemTime) -> Self {
        Self {
            address,
            name,
            rssi: None,
            temperature: None,
            humidity: None,
            battery: None,
            voltage: None,
            last_seen: seen,
            encrypted: false,
            last_sample_packet: None,
            history: VecDeque::new(),
        }
    }

    /// Measurements, oldest first.
    pub fn history(&self) -> &VecDeque<Sample> {
        &self.history
    }

    /// Applies one packet's objects; returns the new sample if this was a new measurement.
    fn apply(&mut self, objects: &[Object], at: SystemTime) -> Option<Sample> {
        let mut packet_id = None;
        let mut temperature = None;
        let mut humidity = None;
        for object in objects {
            match *object {
                Object::PacketId(id) => packet_id = Some(id),
                Object::Temperature(t) => temperature = Some(t),
                Object::Humidity(h) => humidity = Some(h),
                Object::Battery(b) => self.battery = Some(b),
                Object::Voltage(v) => self.voltage = Some(v),
                _ => {}
            }
        }
        self.temperature = temperature.or(self.temperature);
        self.humidity = humidity.or(self.humidity);

        let (Some(temperature), Some(humidity)) = (temperature, humidity) else {
            return None;
        };
        // Devices repeat each measurement in several adverts with the same packet id.
        if packet_id.is_some() && packet_id == self.last_sample_packet {
            return None;
        }
        self.last_sample_packet = packet_id;
        let sample = Sample {
            at,
            temperature,
            humidity,
        };
        self.push(sample);
        Some(sample)
    }

    fn push(&mut self, sample: Sample) {
        self.history.push_back(sample);
        while self
            .history
            .front()
            .is_some_and(|s| sample.at.duration_since(s.at).unwrap_or_default() > HISTORY_SPAN)
        {
            self.history.pop_front();
        }
    }
}

/// All known devices, ordered by address so the order (and colors) are stable between runs.
#[derive(Debug, Default)]
pub struct Devices {
    by_address: BTreeMap<Address, Device>,
}

impl Devices {
    /// Decodes an advert and updates the sending device.
    ///
    /// Returns the new measurement when the advert carried one not seen before.
    ///
    /// # Errors
    ///
    /// Returns the decode error if the payload is not valid BTHome v2. The device's
    /// name, signal strength and last-seen time are still updated.
    pub fn record(&mut self, advert: Advert) -> Result<Option<Measurement>, ParseError> {
        let device = self
            .by_address
            .entry(advert.address)
            .or_insert_with(|| Device::new(advert.address, advert.name.clone(), advert.received));
        device.name = advert.name;
        device.rssi = advert.rssi.or(device.rssi);
        device.last_seen = advert.received;

        let objects = match bthome::parse(&advert.payload)?.payload {
            Payload::Encrypted(_) => {
                device.encrypted = true;
                return Ok(None);
            }
            Payload::Plain(objects) => objects,
        };
        device.encrypted = false;
        let measurement = device
            .apply(&objects, advert.received)
            .map(|sample| Measurement {
                address: device.address,
                name: device.name.clone(),
                sample,
                battery: device.battery,
                voltage: device.voltage,
            });
        Ok(measurement)
    }

    /// Adds a measurement loaded from disk. Call in chronological order.
    pub fn restore(&mut self, measurement: Measurement) {
        let Measurement {
            address,
            name,
            sample,
            battery,
            voltage,
        } = measurement;
        let device = self
            .by_address
            .entry(address)
            .or_insert_with(|| Device::new(address, name.clone(), sample.at));
        device.name = name;
        device.temperature = Some(sample.temperature);
        device.humidity = Some(sample.humidity);
        device.battery = battery.or(device.battery);
        device.voltage = voltage.or(device.voltage);
        device.last_seen = sample.at;
        device.push(sample);
    }

    pub fn iter(&self) -> impl Iterator<Item = &Device> {
        self.by_address.values()
    }

    pub fn len(&self) -> usize {
        self.by_address.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_address.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ADDR: Address = Address([0xa4, 0xc1, 0x38, 0x84, 0x7e, 0xfc]);

    fn advert(payload: &[u8], received: SystemTime) -> Advert {
        Advert {
            address: ADDR,
            name: "ATC_847EFC".to_owned(),
            rssi: Some(-60),
            payload: payload.to_vec(),
            received,
        }
    }

    fn device(devices: &Devices) -> &Device {
        devices.iter().next().expect("one device recorded")
    }

    // pvvx sends measurement and voltage packets alternately, each repeated, sharing a packet id.
    const MEASUREMENT: [u8; 11] = [
        0x40, 0x00, 0xa4, 0x01, 0x57, 0x02, 0xc4, 0x08, 0x03, 0x3e, 0x16,
    ];
    const VOLTAGE: [u8; 10] = [0x40, 0x00, 0xa4, 0x0c, 0x91, 0x0b, 0x10, 0x00, 0x11, 0x01];

    #[test]
    fn merges_alternating_packets() {
        let mut devices = Devices::default();
        let now = SystemTime::now();
        devices.record(advert(&VOLTAGE, now)).expect("valid");
        let measurement = devices
            .record(advert(&MEASUREMENT, now))
            .expect("valid")
            .expect("new measurement");
        assert_eq!(measurement.voltage, Some(2.961));
        assert_eq!(measurement.battery, Some(87));
        let d = device(&devices);
        assert_eq!(d.temperature, Some(22.44));
        assert_eq!(d.humidity, Some(56.94));
    }

    #[test]
    fn repeated_packet_id_adds_one_sample() {
        let mut devices = Devices::default();
        let now = SystemTime::now();
        let first = devices.record(advert(&MEASUREMENT, now)).expect("valid");
        let repeat = devices
            .record(advert(&MEASUREMENT, now + Duration::from_secs(2)))
            .expect("valid");
        assert!(first.is_some());
        assert!(repeat.is_none());
        assert_eq!(device(&devices).history().len(), 1);

        let mut next = MEASUREMENT;
        next[2] = 0xa5;
        devices
            .record(advert(&next, now + Duration::from_secs(10)))
            .expect("valid");
        assert_eq!(device(&devices).history().len(), 2);
    }

    #[test]
    fn invalid_payload_still_marks_device_seen() {
        let mut devices = Devices::default();
        let now = SystemTime::now();
        assert!(devices.record(advert(&[0x20], now)).is_err());
        assert_eq!(device(&devices).last_seen, now);
        assert_eq!(device(&devices).temperature, None);
    }

    #[test]
    fn restore_fills_history_and_latest_values() {
        let mut devices = Devices::default();
        let start = SystemTime::now() - Duration::from_secs(60);
        for (i, t) in [21.0, 21.5].into_iter().enumerate() {
            devices.restore(Measurement {
                address: ADDR,
                name: "Kitchen".to_owned(),
                sample: Sample {
                    at: start + Duration::from_secs(10 * i as u64),
                    temperature: t,
                    humidity: 50.0,
                },
                battery: Some(90),
                voltage: None,
            });
        }
        let d = device(&devices);
        assert_eq!(d.history().len(), 2);
        assert_eq!(d.temperature, Some(21.5));
        assert_eq!(d.name, "Kitchen");
        assert_eq!(d.last_seen, start + Duration::from_secs(10));
    }
}
