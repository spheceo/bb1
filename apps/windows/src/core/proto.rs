//! Control messages, encoded exactly the way Swift's JSONEncoder encodes BB1Core's `ControlMessage`
//! (apps/mac/Sources/BB1Core/Messages.swift): `{"case":{"_0":value}}` for unlabeled associated values,
//! `{"case":{"label":value}}` for labeled ones, `{"case":{}}` for none. `Data` is base64.
//! protocol/vectors.json holds what Swift produces; the tests below check against it.

use serde::{Deserialize, Serialize};

/// `Data` as base64, the way Swift encodes it.
mod b64 {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&STANDARD.encode(v))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(d)?;
        STANDARD.decode(s.as_bytes()).map_err(serde::de::Error::custom)
    }
}

mod b64opt {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &Option<Vec<u8>>, s: S) -> Result<S::Ok, S::Error> {
        match v {
            Some(v) => s.serialize_str(&STANDARD.encode(v)),
            None => s.serialize_none(),
        }
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Vec<u8>>, D::Error> {
        match Option::<String>::deserialize(d)? {
            Some(s) => STANDARD.decode(s.as_bytes()).map(Some).map_err(serde::de::Error::custom),
            None => Ok(None),
        }
    }
}

/// Swift's wrapper for an unlabeled associated value.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct V<T> {
    #[serde(rename = "_0")]
    pub v: T,
}

/// `Data` as an unlabeled associated value: `{"_0":"base64"}`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct VData {
    #[serde(rename = "_0", with = "b64")]
    pub v: Vec<u8>,
}

pub fn v<T>(v: T) -> V<T> {
    V { v }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ClientHello {
    pub version: i64,
    #[serde(with = "b64")]
    pub nonce: Vec<u8>,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub device_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default, with = "b64opt")]
    pub ecdh_pub: Option<Vec<u8>>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub app_build: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub purpose: Option<String>,
    /// Codecs this client decodes. Windows: only "h264".
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub codecs: Option<Vec<String>>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HostHello {
    pub version: i64,
    #[serde(with = "b64")]
    pub nonce: Vec<u8>,
    #[serde(with = "b64")]
    pub proof: Vec<u8>,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub host_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default, with = "b64opt")]
    pub ecdh_pub: Option<Vec<u8>>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub app_build: Option<i64>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PairedMsg {
    pub host_id: String,
    pub host_name: String,
    #[serde(with = "b64")]
    pub secret: Vec<u8>,
}

pub const CODEC_H264: u8 = 1;
pub const CODEC_HEVC: u8 = 2;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StreamConfig {
    pub width: i64,
    pub height: i64,
    /// 1 = H.264, 2 = HEVC
    pub codec: u8,
    pub fps: i64,
    pub bitrate_mbps: i64,
    pub low_latency_encoder: bool,
    pub udp_port: u16,
    pub session_id: u32,
    pub epoch: u16,
}

impl StreamConfig {
    pub fn codec_name(&self) -> &'static str {
        if self.codec == CODEC_HEVC { "HEVC" } else { "H.264" }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PingMsg {
    pub seq: u32,
    pub client_us: u64,
    pub received: u64,
    pub decoded: u64,
    pub displayed: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PongMsg {
    pub seq: u32,
    pub client_us: u64,
    pub host_us: u64,
    pub captured: u64,
    pub encoded: u64,
    pub sent: u64,
    pub encode_avg_us: u64,
    pub host_recoveries: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CursorMsg {
    #[serde(with = "b64")]
    pub png: Vec<u8>,
    pub width: f64,
    pub height: f64,
    pub hot_x: f64,
    pub hot_y: f64,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct ClipboardMsg {
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default, with = "b64opt")]
    pub png: Option<Vec<u8>>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct NackMsg {
    pub frame_id: u32,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub indices: Option<Vec<u16>>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct KeyMsg {
    /// macOS virtual key code
    pub code: u16,
    pub down: bool,
    pub is_repeat: bool,
}

/// Positions are 0...1 relative to the remote display, top-left origin.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct MouseMoveMsg {
    pub x: f64,
    pub y: f64,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct MouseButtonMsg {
    /// 0 left, 1 right, 2+ other
    pub button: i64,
    pub down: bool,
    pub clicks: i64,
    pub x: f64,
    pub y: f64,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct ScrollMsg {
    pub dx: f64,
    pub dy: f64,
    /// true: pixels (touchpad), false: lines (wheel)
    pub precise: bool,
    /// NSEvent.Phase raw values (0 for a wheel)
    pub phase: u64,
    pub momentum: u64,
}

/// Host → client: a small picture of the host's main screen, for the Macs list.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PreviewMsg {
    /// None when there's no picture (no Screen Recording permission yet).
    #[serde(skip_serializing_if = "Option::is_none", default, with = "b64opt")]
    pub jpeg: Option<Vec<u8>>,
    pub capture_ms: f64,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatusMsg {
    pub phase: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub progress: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub detail: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum Msg {
    // Handshake (plaintext)
    ClientHello(V<ClientHello>),
    HostHello(V<HostHello>),
    ClientProof(VData),
    AuthFailed(V<String>),

    // Sealed
    Welcome {
        #[serde(rename = "hostName")]
        host_name: String,
    },
    StreamConfig(V<StreamConfig>),
    RequestKeyframe { reason: String },
    Ping(V<PingMsg>),
    Pong(V<PongMsg>),
    Key(V<KeyMsg>),
    MouseMove(V<MouseMoveMsg>),
    MouseButton(V<MouseButtonMsg>),
    Scroll(V<ScrollMsg>),
    ReleaseAll {},
    SetCursorInVideo(V<bool>),
    Notice(V<String>),
    Bye(V<String>),

    // Version 2
    Nack(V<NackMsg>),
    ClientLog(V<String>),
    Clipboard(V<ClipboardMsg>),

    // Version 3
    SetAudio(V<bool>),

    // Version 4
    Paired(V<PairedMsg>),
    Cursor(V<CursorMsg>),
    CapsLock(V<bool>),

    UpdateStatus(V<UpdateStatusMsg>),

    // Screen previews, on "update" connections
    RequestPreview {
        #[serde(rename = "maxWidth")]
        max_width: i64,
    },
    Preview(V<PreviewMsg>),
}

impl Msg {
    pub fn encode(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("control messages always encode")
    }

    /// None for message types this client doesn't know (a newer host may send them).
    pub fn decode(bytes: &[u8]) -> Option<Msg> {
        serde_json::from_slice(bytes).ok()
    }

    pub fn key(code: u16, down: bool, is_repeat: bool) -> Msg {
        Msg::Key(v(KeyMsg { code, down, is_repeat }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vectors() -> serde_json::Value {
        let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../protocol/vectors.json")).unwrap();
        serde_json::from_str(&text).unwrap()
    }

    /// Every message Swift wrote decodes here, and encodes back to the same JSON.
    #[test]
    fn messages_match_swift() {
        let v = vectors();
        for json in v["messages"].as_array().unwrap() {
            let json = json.as_str().unwrap();
            let msg = Msg::decode(json.as_bytes()).unwrap_or_else(|| panic!("couldn't decode {json}"));
            let ours: serde_json::Value = serde_json::from_slice(&msg.encode()).unwrap();
            let theirs: serde_json::Value = serde_json::from_str(json).unwrap();
            assert_eq!(ours, theirs, "{json}");
        }
    }

    #[test]
    fn unknown_messages_are_skipped() {
        assert!(Msg::decode(br#"{"somethingNew":{"_0":1}}"#).is_none());
        assert!(Msg::decode(br#"{"welcome":{"hostName":"x","extra":1}}"#).is_some());
    }
}
