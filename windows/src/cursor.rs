//! Cursor images arrive on TCP; each viewer receives its own position on UDP.

use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::Value;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// Premultiplied BGRA, top row first.
pub struct CursorImage {
    pub w: u32,
    pub h: u32,
    pub hx: i32,
    pub hy: i32,
    pub bgra: Vec<u8>,
}

#[derive(Default)]
pub struct CursorStore {
    images: Mutex<VecDeque<(u16, Arc<CursorImage>)>>,
}

impl CursorStore {
    pub fn insert(&self, id: u16, image: CursorImage) {
        let mut images = self.images.lock().unwrap_or_else(|e| e.into_inner());
        images.retain(|(i, _)| *i != id);
        images.push_back((id, Arc::new(image)));
        if images.len() > 32 {
            images.pop_front();
        }
    }

    pub fn get(&self, id: u16) -> Option<Arc<CursorImage>> {
        if let Some((_, image)) = self.latest().filter(|(i, _)| *i == id) {
            return Some(image);
        }
        self.images.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|(i, _)| *i == id).map(|(_, image)| image.clone())
    }

    pub fn latest(&self) -> Option<(u16, Arc<CursorImage>)> {
        self.images.lock().unwrap_or_else(|e| e.into_inner()).back().cloned()
    }
}

pub fn decode_shape(v: &Value) -> Option<(u16, CursorImage)> {
    let id = u16::try_from(v.get("id")?.as_u64()?).ok()?;
    let w = v.get("w")?.as_u64()?;
    let h = v.get("h")?.as_u64()?;
    if !(1..=128).contains(&w) || !(1..=128).contains(&h) {
        return None;
    }
    let hx = v.get("hx")?.as_i64()?.clamp(0, w as i64 - 1) as i32;
    let hy = v.get("hy")?.as_i64()?.clamp(0, h as i64 - 1) as i32;
    let encoded = v.get("bgra")?.as_str()?;
    let len = (w * h * 4) as usize;
    if encoded.len() != len.div_ceil(3) * 4 {
        return None;
    }
    let bgra = STANDARD.decode(encoded).ok()?;
    if bgra.len() != len {
        return None;
    }
    Some((id, CursorImage { w: w as u32, h: h as u32, hx, hy, bgra }))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CursorState {
    pub seq: u16,
    pub visible: bool,
    pub on_display: bool,
    pub x: i16,
    pub y: i16,
    pub shape: u16,
}

pub fn parse_state(payload: &[u8]) -> Option<CursorState> {
    if payload.len() != 10 || payload[0] != 0x02 {
        return None;
    }
    Some(CursorState {
        seq: u16::from_be_bytes([payload[2], payload[3]]),
        visible: payload[1] & 1 != 0,
        on_display: payload[1] & 2 != 0,
        x: i16::from_be_bytes([payload[4], payload[5]]),
        y: i16::from_be_bytes([payload[6], payload[7]]),
        shape: u16::from_be_bytes([payload[8], payload[9]]),
    })
}

pub fn seq_newer(a: u16, b: u16) -> bool {
    (a.wrapping_sub(b) as i16) > 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn shape() -> Value {
        json!({"id": 7, "w": 2, "h": 1, "hx": -4, "hy": 9, "bgra": STANDARD.encode([0, 0, 0, 0, 10, 20, 30, 255])})
    }

    #[test]
    fn valid_shape_clamps_hotspot() {
        let (id, image) = decode_shape(&shape()).unwrap();
        assert_eq!(id, 7);
        assert_eq!((image.w, image.h, image.hx, image.hy), (2, 1, 0, 0));
        assert_eq!(image.bgra, [0, 0, 0, 0, 10, 20, 30, 255]);
    }

    #[test]
    fn invalid_shapes() {
        for (key, value) in [
            ("bgra", json!(STANDARD.encode([0; 7]))),
            ("bgra", json!("!!!!!!!!!!!!")),
            ("w", json!(129)),
            ("h", json!(129)),
            ("w", json!(u64::MAX)),
            ("h", json!(0)),
            ("id", json!(65536)),
            ("id", json!(-1)),
            ("hx", json!(null)),
            ("w", json!(1.5)),
        ] {
            let mut v = shape();
            v[key] = value;
            assert!(decode_shape(&v).is_none(), "{key}: {v}");
        }
        for v in [json!(null), json!([]), json!("shape"), json!({})] {
            assert!(decode_shape(&v).is_none());
        }
    }

    #[test]
    fn maximum_shape_and_extreme_hotspots() {
        let v = json!({"id": 65535, "w": 128, "h": 128, "hx": i64::MAX, "hy": i64::MIN, "bgra": STANDARD.encode(vec![0; 128 * 128 * 4])});
        let (id, image) = decode_shape(&v).unwrap();
        assert_eq!(id, 65535);
        assert_eq!((image.w, image.h, image.hx, image.hy), (128, 128, 127, 0));
        assert_eq!(image.bgra.len(), 128 * 128 * 4);
    }

    #[test]
    fn state_packets() {
        let bytes = [2, 3, 0x12, 0x34, 0xff, 0xfe, 0, 42, 0xab, 0xcd];
        assert_eq!(parse_state(&bytes), Some(CursorState { seq: 0x1234, visible: true, on_display: true, x: -2, y: 42, shape: 0xabcd }));
        assert!(parse_state(&bytes[..9]).is_none());
        assert!(parse_state(&[0; 10]).is_none());
        assert!(parse_state(&[2; 11]).is_none());
        assert!(parse_state(&[]).is_none());
        let mut hidden = bytes;
        hidden[1] = 0;
        let state = parse_state(&hidden).unwrap();
        assert!(!state.visible && !state.on_display);
    }

    #[test]
    fn sequence_wraparound() {
        assert!(seq_newer(1, 0));
        assert!(seq_newer(0, 65535));
        assert!(seq_newer(2, 65534));
        assert!(!seq_newer(65535, 0));
        assert!(!seq_newer(10, 10));
        assert!(!seq_newer(0x8000, 0));
    }

    #[test]
    fn store_keeps_recent_shapes() {
        let store = CursorStore::default();
        assert!(store.latest().is_none());
        for id in 0..33 {
            store.insert(id, decode_shape(&shape()).unwrap().1);
        }
        assert!(store.get(0).is_none());
        let previous = store.get(1).unwrap();
        store.insert(1, decode_shape(&shape()).unwrap().1);
        assert_eq!(store.latest().unwrap().0, 1);
        assert!(!Arc::ptr_eq(&previous, &store.get(1).unwrap()));
        store.insert(33, decode_shape(&shape()).unwrap().1);
        assert!(store.get(2).is_none());
        assert_eq!(previous.w, 2);
    }
}
