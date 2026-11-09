//! Property tests for core model serde + scheme enums (smallest data types).

#![cfg(test)]

use proptest::prelude::*;

use crate::models::{ContentKind, SourceKind};
use crate::protocol::{DeliveryKind, IngestKind, StreamScheme};

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn stream_scheme_serde_roundtrip(raw in "\\PC{0,32}") {
        let s = StreamScheme::parse(&raw);
        let j = serde_json::to_string(&s).expect("serialize");
        let back: StreamScheme = serde_json::from_str(&j).expect("deserialize");
        prop_assert_eq!(s, back);
    }

    #[test]
    fn delivery_detect_total(raw in "\\PC{0,120}") {
        let _ = DeliveryKind::detect(&raw);
    }
}

#[test]
fn content_kind_serde_all() {
    for k in [ContentKind::Live, ContentKind::Vod, ContentKind::Series] {
        let j = serde_json::to_string(&k).unwrap();
        let back: ContentKind = serde_json::from_str(&j).unwrap();
        assert_eq!(k, back);
    }
}

#[test]
fn source_kind_serde_all() {
    for k in [
        SourceKind::M3u,
        SourceKind::M3uPlus,
        SourceKind::Xtream,
        SourceKind::Stalker,
        SourceKind::Xmltv,
        SourceKind::DirectUrl,
    ] {
        let j = serde_json::to_string(&k).unwrap();
        let back: SourceKind = serde_json::from_str(&j).unwrap();
        assert_eq!(k, back);
        assert!(!k.label().is_empty());
    }
}

#[test]
fn ingest_kind_serde_all() {
    for k in [
        IngestKind::Rtmp,
        IngestKind::Srt,
        IngestKind::Rtsp,
        IngestKind::Whip,
        IngestKind::RtpUdp,
        IngestKind::Unknown,
    ] {
        let j = serde_json::to_string(&k).unwrap();
        let back: IngestKind = serde_json::from_str(&j).unwrap();
        assert_eq!(k, back);
    }
}
