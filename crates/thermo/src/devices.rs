//! Latest readings and history of every BTHome device heard so far.

use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, SystemTime};

use bluer::Address;
use bthome::{Object, ParseError, Payload};

use crate::scanner::Advert;

/// How far back history is kept in memory: the longest calibration window.
pub const HISTORY_SPAN: Duration = Duration::from_secs(48 * 60 * 60);

/// Where a sample came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Source {
    /// Received in an advert.
    Live,
    /// Downloaded from the device's own log: an average over a longer period.
    Log,
}

/// One temperature and humidity measurement.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    pub at: SystemTime,
    pub temperature: f32,
    pub humidity: f32,
    pub source: Source,
}

/// A record from a device's log, already mapped to host time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LoggedSample {
    /// Middle of the averaging period.
    pub at: SystemTime,
    pub temperature: f32,
    pub humidity: f32,
    pub voltage: f32,
    /// Length of the averaging period.
    pub span: Duration,
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
            source: Source::Live,
        };
        self.insert(sample);
        Some(sample)
    }

    /// Inserts in time order and drops samples older than [`HISTORY_SPAN`] before the newest.
    fn insert(&mut self, sample: Sample) {
        let position = self.history.partition_point(|s| s.at <= sample.at);
        self.history.insert(position, sample);
        let newest = self.history.back().map_or(sample.at, |s| s.at);
        while self
            .history
            .front()
            .is_some_and(|s| newest.duration_since(s.at).unwrap_or_default() > HISTORY_SPAN)
        {
            self.history.pop_front();
        }
    }

    /// Whether any sample lies within `radius` of `at`.
    fn has_sample_near(&self, at: SystemTime, radius: Duration) -> bool {
        let from = at.checked_sub(radius).unwrap_or(at);
        let first = self.history.partition_point(|s| s.at < from);
        self.history.get(first).is_some_and(|s| s.at <= at + radius)
    }

    /// Whether history between `since` and `until` has a hole longer than `max_gap`.
    pub fn has_gap(&self, since: SystemTime, until: SystemTime, max_gap: Duration) -> bool {
        let first = self.history.partition_point(|s| s.at < since);
        let mut previous = since;
        for sample in self.history.range(first..).take_while(|s| s.at <= until) {
            if sample.at.duration_since(previous).unwrap_or_default() > max_gap {
                return true;
            }
            previous = sample.at;
        }
        until.duration_since(previous).unwrap_or_default() > max_gap
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
        if sample.at >= device.last_seen {
            device.temperature = Some(sample.temperature);
            device.humidity = Some(sample.humidity);
            device.battery = battery.or(device.battery);
            device.voltage = voltage.or(device.voltage);
        }
        device.last_seen = device.last_seen.max(sample.at);
        device.insert(sample);
    }

    /// Adds downloaded log records where history has no sample within half a record's
    /// span; returns the added ones so they can be saved.
    pub fn merge_log(&mut self, address: Address, records: &[LoggedSample]) -> Vec<Measurement> {
        let Some(device) = self.by_address.get_mut(&address) else {
            return Vec::new();
        };
        let mut added = Vec::new();
        for record in records {
            if device.has_sample_near(record.at, record.span / 2) {
                continue;
            }
            let sample = Sample {
                at: record.at,
                temperature: record.temperature,
                humidity: record.humidity,
                source: Source::Log,
            };
            device.insert(sample);
            added.push(Measurement {
                address,
                name: device.name.clone(),
                sample,
                battery: None,
                voltage: Some(record.voltage),
            });
        }
        added
    }

    pub fn get(&self, address: Address) -> Option<&Device> {
        self.by_address.get(&address)
    }

    pub fn rename(&mut self, address: Address, name: String) {
        if let Some(device) = self.by_address.get_mut(&address) {
            device.name = name;
        }
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
                    source: Source::Live,
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

    #[test]
    fn log_records_fill_only_gaps() {
        let mut devices = Devices::default();
        let start = SystemTime::now() - Duration::from_secs(3 * 3600);
        devices.record(advert(&MEASUREMENT, start)).expect("valid");
        let half_hour = Duration::from_secs(1800);
        let records: Vec<LoggedSample> = (0..4)
            .map(|i| LoggedSample {
                at: start + half_hour * i,
                temperature: 20.0,
                humidity: 50.0,
                voltage: 2.9,
                span: half_hour,
            })
            .collect();
        let d = device(&devices);
        assert!(d.has_gap(start, start + half_hour * 3, Duration::from_secs(900)));

        let added = devices.merge_log(ADDR, &records);
        // The first record overlaps the live sample; the other three fill the gap.
        assert_eq!(added.len(), 3);
        let d = device(&devices);
        let times: Vec<SystemTime> = d.history().iter().map(|s| s.at).collect();
        assert!(times.is_sorted());
        assert_eq!(d.history()[1].source, Source::Log);
        // Live values stay those of the newest live advert.
        assert_eq!(d.temperature, Some(22.44));
    }
}
