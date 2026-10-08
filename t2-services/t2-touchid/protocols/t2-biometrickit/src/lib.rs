// SPDX-License-Identifier: GPL-3.0-or-later

//! BiometricKit protocol for the Apple T2 Touch ID sensor.
//!
//! The sensor hangs on the SEP endpoint `sbio` inside bridgeOS and is reachable
//! only over BridgeXPC on the CDC-NCM link, never through the host's SEP
//! mailbox. This crate speaks that protocol and makes no policy decisions: it
//! reports which enrolled finger matched, nothing more.

pub mod proto;
pub mod wire;

use std::net::{Ipv6Addr, SocketAddrV6, TcpStream};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use plist::Value;
use t2_bridgexpc::{discovery, remote};

pub const SERVICE: &str = "com.apple.eos.BiometricKit";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Identity {
    pub slot: usize,
    pub user_id: u32,
    pub uuid: [u8; 16],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    FingerDown,
    FingerUp,
    Status(u32),
    Statistics(usize),
    /// The SEP's verdict. `slot` names the enrolled finger that matched.
    MatchResult { slot: Option<usize>, bytes: usize },
    Other { kind: u32, bytes: usize },
}

pub struct Session {
    stream: TcpStream,
    identities: Vec<Identity>,
    records: Vec<u8>,
}

/// Discover only the advertised service port. No BiometricKit session,
/// sensor command or SEP mailbox operation is issued. Discovery finishes and
/// closes its RemoteXPC connection before returning.
pub fn discover_service_port(interface: Option<String>, host: Option<String>) -> Result<u16> {
    let interface = discovery::interface(interface)?;
    let host = discovery::host(&interface, host)?;
    Ok(remote::discover_direct_named_service(&interface, host, SERVICE)
        .context("BiometricKit discovery failed")?.service_port)
}

/// Query the account's secure-key-store state without sensor initialization.
/// The reply is a raw firmware value, not a fingerprint authentication result.
/// No reset, calibration, identity inventory or SEP mailbox operation is sent.
pub fn query_sks_lock_state(
    interface: Option<String>, host: Option<String>, user_id: u32,
) -> Result<u32> {
    ensure!(user_id != 0, "an explicit macOS user id is required");
    let interface = discovery::interface(interface)?;
    let host = discovery::host(&interface, host)?;
    let service = remote::discover_direct_named_service(&interface, host, SERVICE)
        .context("BiometricKit discovery failed")?;
    Session::connect(&interface, host, service.service_port)?.sks_lock_state(user_id)
}

/// Use the port discovered before native setup; do not perform another
/// RemoteXPC discovery while the native transport holds SEP DMA.
pub fn query_sks_lock_state_at(
    interface: Option<String>, host: Option<String>, user_id: u32, port: u16,
) -> Result<u32> {
    ensure!(user_id != 0, "an explicit macOS user id is required");
    ensure!((discovery::FIRST_DYNAMIC_PORT..=discovery::LAST_DYNAMIC_PORT).contains(&port),
        "invalid BiometricKit service port");
    let interface = discovery::interface(interface)?;
    let host = discovery::host(&interface, host)?;
    Session::connect(&interface, host, port)?.sks_lock_state(user_id)
}


impl Session {
    /// Find the link, activate BiometricKit and bring the sensor up to the
    /// point where it can scan. Without the calibration load the sensor stays
    /// dark, so it is part of opening a session rather than a separate step.
    pub fn open(interface: Option<String>, host: Option<String>) -> Result<Self> {
        let interface = discovery::interface(interface)?;
        let host = discovery::host(&interface, host)?;
        let service = remote::discover_direct_named_service(&interface, host, SERVICE)
            .context("BiometricKit was not advertised")?;
        let mut session = Self::connect(&interface, host, service.service_port)?;
        session.start()?;
        Ok(session)
    }

    pub fn connect(interface: &str, host: Ipv6Addr, port: u16) -> Result<Self> {
        let scope = unsafe {
            let name = std::ffi::CString::new(interface)?;
            libc::if_nametoindex(name.as_ptr())
        };
        ensure!(scope != 0, "unknown interface {interface}");
        let address = SocketAddrV6::new(host, port, 0, scope);
        let stream = TcpStream::connect_timeout(&address.into(), Duration::from_secs(3))?;
        Self::from_stream(stream)
    }

    fn from_stream(mut stream: TcpStream) -> Result<Self> {
        stream.set_nodelay(true)?;
        // Apply the bound before HELO, not only after the first command.
        stream.set_read_timeout(Some(Duration::from_secs(8)))?;
        stream.set_write_timeout(Some(Duration::from_secs(8)))?;
        wire::handshake(&mut stream, "t2-touchid")?;
        Ok(Self { stream, identities: Vec::new(), records: Vec::new() })
    }

    fn start(&mut self) -> Result<()> {
        let opened = self.request(Value::Array(vec![Value::Integer(1.into())]))?;
        ensure!(proto::is_ok(&opened), "BiometricKit service did not open");
        self.command(proto::RESET_SENSOR, 2, &[], 0)?;
        self.command(proto::CANCEL, 0, &[], 0)?;

        let ready = self.command(proto::READINESS, 0, &[], 1)?;
        let ready = proto::data(&ready)?;
        ensure!(ready.first() == Some(&1), "sensor is not ready");

        let calibration = self.request(Value::Array(vec![Value::Integer(11.into())]))?;
        let calibration = match calibration.as_array().and_then(|items| items.first()) {
            Some(Value::Data(bytes)) if !bytes.is_empty() => bytes.clone(),
            _ => bail!("bridgeOS returned no FDR calibration"),
        };
        let loaded = self.command(proto::LOAD_CALIBRATION, 3, &calibration, 0)?;
        ensure!(proto::is_ok(&loaded), "calibration load rejected");

        let version = Value::Array(vec![Value::Integer(10.into()), Value::Integer(2.into())]);
        ensure!(proto::is_ok(&self.request(version)?), "setClientVersion failed");
        Ok(())
    }

    /// Enrolled fingers for a macOS user id, in slot order.
    pub fn read_identities(&mut self, user_id: u32) -> Result<&[Identity]> {
        let capacity = (proto::IDENTITY_RECORD_SIZE * proto::MAX_IDENTITIES) as u32;
        let reply = self.command(
            proto::USER_IDENTITIES,
            0,
            &user_id.to_le_bytes(),
            capacity,
        )?;
        let records = proto::data(&reply)?;
        ensure!(
            records.len() <= capacity as usize
                && records.len() % proto::IDENTITY_RECORD_SIZE == 0,
            "malformed identity inventory: {} bytes",
            records.len()
        );
        self.identities.clear();
        for (slot, record) in records.chunks_exact(proto::IDENTITY_RECORD_SIZE).enumerate() {
            let prefix = u32::from_le_bytes(record[0..4].try_into().unwrap());
            let suffix = u32::from_le_bytes(record[16..20].try_into().unwrap());
            let uuid = if prefix == user_id {
                &record[4..20]
            } else if suffix == user_id {
                &record[0..16]
            } else {
                continue;
            };
            self.identities.push(Identity {
                slot,
                user_id,
                uuid: uuid.try_into().unwrap(),
            });
        }
        self.records = records;
        Ok(&self.identities)
    }

    pub fn identities(&self) -> &[Identity] {
        &self.identities
    }

    pub fn provisioning_state(&mut self, user_id: u32) -> Result<u32> {
        let reply = self.command(proto::PROVISIONING_STATE, 0, &[], 64)?;
        let bytes = match proto::data(&reply) {
            Ok(bytes) if bytes.len() >= 4 => bytes,
            _ => proto::data(&self.command(
                proto::PROVISIONING_STATE,
                0,
                &user_id.to_le_bytes(),
                64,
            )?)?,
        };
        ensure!(bytes.len() >= 4, "short provisioning state");
        Ok(u32::from_le_bytes(bytes[0..4].try_into().unwrap()))
    }

    pub fn sks_lock_state(&mut self, user_id: u32) -> Result<u32> {
        let reply = self.command(proto::SKS_LOCK_STATE, 0, &user_id.to_le_bytes(), 4)?;
        let bytes = proto::data(&reply)?;
        ensure!(bytes.len() == 4, "invalid lock state length: {}", bytes.len());
        Ok(u32::from_le_bytes(bytes[0..4].try_into().unwrap()))
    }

    pub fn start_presence(&mut self) -> Result<()> {
        let reply = self.command(proto::PRESENCE, 0, &[], 0)?;
        ensure!(proto::is_ok(&reply), "presence request rejected");
        Ok(())
    }

    /// Start a verify-only match against the identities read earlier.
    pub fn start_match(&mut self, flags: u32, credential_set: u32) -> Result<()> {
        let payload = proto::match_init(flags, credential_set, &self.records);
        let reply = self.command(proto::MATCH, 0, &payload, 0)?;
        if !proto::is_ok(&reply) {
            bail!("match rejected: status {:#010x}", proto::status(&reply) as u32);
        }
        Ok(())
    }

    pub fn cancel(&mut self) -> Result<()> {
        self.command(proto::CANCEL, 0, &[], 0)?;
        Ok(())
    }

    /// Wait for the next sensor event, acknowledging it as bridgeOS expects.
    pub fn next_event(&mut self, timeout: Duration) -> Result<Option<Event>> {
        self.stream.set_read_timeout(Some(timeout))?;
        let incoming = match wire::receive_plist(&mut self.stream) {
            Ok(value) => value,
            Err(error) => {
                if let Some(io) = error.downcast_ref::<std::io::Error>()
                    && matches!(
                        io.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    )
                {
                    return Ok(None);
                }
                return Err(error);
            }
        };
        let (is_reply, id, body) = split_envelope(&incoming)?;
        ensure!(!is_reply, "unexpected reply while waiting for events");
        self.acknowledge(&id)?;
        Ok(Some(self.decode_event(&body)))
    }

    fn command(&mut self, command: u16, value: u16, data: &[u8], capacity: u32) -> Result<Value> {
        self.request(proto::command_payload(command, value, data, capacity))
    }

    /// Send one request and pump the event stream until its reply arrives.
    /// bridgeOS expects every event to be acknowledged, so they cannot simply
    /// be dropped while waiting.
    fn request(&mut self, payload: Value) -> Result<Value> {
        let id = uuid::Uuid::new_v4().to_string().to_uppercase();
        wire::send_plist(&mut self.stream, &envelope(&id, false, payload))?;
        // Commands reply promptly; a long block here only happens on a link
        // that died under us (a suspend), so keep it short enough that the
        // caller notices well within fprintd's own wait rather than after 30s.
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            ensure!(!remaining.is_zero(), "BiometricKit command reply timed out");
            self.stream.set_read_timeout(Some(remaining))?;
            let incoming = wire::receive_plist(&mut self.stream)?;
            let (is_reply, received, body) = split_envelope(&incoming)?;
            if is_reply {
                ensure!(received == id, "reply for a different request");
                return Ok(body);
            }
            self.acknowledge(&received)?;
        }
    }

    fn acknowledge(&mut self, id: &str) -> Result<()> {
        let ack = envelope(id, true, Value::Array(vec![Value::Integer(0.into())]));
        wire::send_plist(&mut self.stream, &ack)
    }

    fn decode_event(&self, body: &Value) -> Event {
        let Some(items) = body.as_array() else {
            return Event::Other { kind: 0, bytes: 0 };
        };
        let method = items.first().and_then(Value::as_unsigned_integer);
        let Some(Value::Data(data)) = items.get(2) else {
            return Event::Other { kind: 0, bytes: 0 };
        };
        if method != Some(9) || data.len() < 24 {
            return Event::Other { kind: 0, bytes: data.len() };
        }
        let kind = u32::from_le_bytes(data[8..12].try_into().unwrap());
        let payload = &data[24..];
        match kind {
            proto::EVENT_STATUS if payload.len() >= 4 => {
                match u32::from_le_bytes(payload[0..4].try_into().unwrap()) {
                    proto::STATUS_FINGER_DOWN => Event::FingerDown,
                    proto::STATUS_FINGER_UP => Event::FingerUp,
                    code => Event::Status(code),
                }
            }
            proto::EVENT_STATS => Event::Statistics(payload.len()),
            proto::EVENT_MATCH => Event::MatchResult {
                slot: self.matched_slot(payload),
                bytes: payload.len(),
            },
            _ => Event::Other { kind, bytes: payload.len() },
        }
    }

    /// The result record carries the matched identity's UUID somewhere inside
    /// it; find which enrolled finger that is.
    fn matched_slot(&self, payload: &[u8]) -> Option<usize> {
        self.identities.iter().position(|identity| {
            payload
                .windows(16)
                .any(|window| window == identity.uuid)
        })
    }
}

fn envelope(id: &str, reply: bool, payload: Value) -> Value {
    Value::Array(vec![
        Value::Integer(1.into()),
        Value::Boolean(reply),
        Value::String(id.to_owned()),
        payload,
    ])
}

fn split_envelope(value: &Value) -> Result<(bool, String, Value)> {
    let items = value.as_array().context("envelope is not an array")?;
    ensure!(items.len() == 4, "malformed envelope");
    let reply = items[1].as_boolean().context("envelope flag missing")?;
    let id = items[2].as_string().context("envelope id missing")?.to_owned();
    Ok((reply, id, items[3].clone()))
}

#[cfg(test)]
mod lock_state_tests {
    use super::*;
    use std::net::TcpListener;
    use std::thread;

    fn query_command(reply: Value, wrong_id: bool, inventory: bool) -> Result<u32> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let client = TcpStream::connect(listener.local_addr()?)?;
        let peer = thread::spawn(move || -> Result<()> {
            let (mut stream, _) = listener.accept()?;
            stream.set_read_timeout(Some(Duration::from_secs(2)))?;
            wire::send(&mut stream, wire::HELO, br#"{"BridgeXPCVersion":39}"#)?;
            ensure!(wire::receive(&mut stream)?.kind == wire::HELO);
            let incoming = wire::receive_plist(&mut stream)?;
            let (is_reply, id, body) = split_envelope(&incoming)?;
            ensure!(!is_reply);
            // The first operation must be only the requested read for UID 501. Opening,
            // resetting or calibrating the sensor would fail this contract.
            let expected = Value::Array(vec![
                Value::Integer(3.into()), Value::Integer(0.into()),
                Value::Data(vec![0x42, 0x4d, if inventory { 0x42 } else { 0x27 }, 0, 1, 0, 0, 0,
                    0xf5, 1, 0, 0]), Value::Integer(if inventory { 200.into() } else { 4.into() }),
            ]);
            ensure!(body == expected, "unexpected sensor operation");
            wire::send_plist(&mut stream, &envelope(
                if wrong_id { "unrelated-request" } else { &id }, true, reply,
            ))?;
            // A status probe ends here; no sensor operation may follow.
            let mut byte = [0u8; 1];
            use std::io::Read;
            ensure!(stream.read(&mut byte)? == 0, "unexpected trailing operation");
            Ok(())
        });
        let mut session = Session::from_stream(client)?;
        let result = if inventory {
            session.read_identities(501).map(|found| found.len() as u32)
        } else {
            session.sks_lock_state(501)
        };
        drop(session);
        peer.join().expect("mock peer panicked")?;
        result
    }

    fn query(reply: Value, wrong_id: bool) -> Result<u32> {
        query_command(reply, wrong_id, false)
    }

    fn reply(status: u64, bytes: Vec<u8>) -> Value {
        Value::Array(vec![Value::Integer(status.into()), Value::Data(bytes)])
    }

    #[test]
    fn status_probe_rejects_automatic_uid_before_discovery() {
        assert!(query_sks_lock_state(None, None, 0).is_err());
    }

    #[test]
    fn cached_status_probe_rejects_invalid_port_before_discovery() {
        for port in [0, 1, 49151] {
            let error = query_sks_lock_state_at(None, None, 501, port).unwrap_err();
            assert!(error.to_string().contains("invalid BiometricKit service port"));
        }
        assert!(query_sks_lock_state_at(None, None, 0, 50000).unwrap_err()
            .to_string().contains("explicit macOS user id"));
    }

    #[test]
    fn status_probe_sends_no_sensor_initialization() {
        assert_eq!(query(reply(0, vec![0x1b, 0, 0, 0]), false).unwrap(), 0x1b);
    }

    #[test]
    fn status_probe_rejects_error_and_malformed_payloads() {
        assert!(query(reply(0xe00002c2, vec![0; 4]), false).is_err());
        for size in [0, 3, 5, 8] {
            assert!(query(reply(0, vec![0; size]), false).is_err());
        }
    }

    #[test]
    fn inventory_query_requires_valid_reply_without_initialization() {
        let mut record = vec![0; 20];
        record[0..4].copy_from_slice(&501u32.to_le_bytes());
        assert_eq!(query_command(reply(0, record), false, true).unwrap(), 1);
        assert_eq!(query_command(reply(0, Vec::new()), false, true).unwrap(), 0);
        assert!(query_command(reply(0xe00002c2, Vec::new()), false, true).is_err());
        for size in [19, 21, 220] {
            assert!(query_command(reply(0, vec![0; size]), false, true).is_err());
        }
        assert!(query_command(reply(0, vec![0; 20]), true, true).is_err());
    }

    #[test]
    fn status_probe_rejects_unrelated_reply() {
        assert!(query(reply(0, vec![0; 4]), true).is_err());
    }
}
