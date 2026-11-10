//! Property tests for protocol routing + HLS body probe + chapter skip.
#![cfg(test)]

use proptest::prelude::*;

use fluxplay_core::protocol::StreamScheme;

use crate::video_frame::yuv_to_rgb_coeffs;
use crate::{
    classify_chapter_title, opening_span_from_chapters, parse_hls_manifest_body, route,
    support_matrix, ChapterClass, ColorMatrix, PixelLayout,
};

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn route_never_panics(raw in "\\PC{0,400}") {
        let _ = route(&raw);
    }

    #[test]
    fn hls_body_never_panics(raw in "\\PC{0,4000}") {
        let probe = parse_hls_manifest_body(&raw);
        prop_assert!(probe.variant_count <= raw.lines().count());
        prop_assert_eq!(probe.bytes, raw.len());
    }

    #[test]
    fn chapter_classify_total(title in "\\PC{0,120}") {
        let _ = classify_chapter_title(&title);
    }

    #[test]
    fn opening_from_fuzz_chapters(
        n in 0usize..12,
        series in any::<bool>(),
    ) {
        let mut chapters = Vec::with_capacity(n);
        for i in 0..n {
            chapters.push((i as f64 * 30.0, format!("ch-{i}")));
        }
        let _ = opening_span_from_chapters(&chapters, series);
    }

    #[test]
    fn pixel_layout_rows_sane(h in 0u32..4096) {
        for layout in [PixelLayout::Rgba, PixelLayout::Nv12, PixelLayout::P010] {
            let rows = layout.rows(h);
            if h == 0 {
                prop_assert_eq!(rows, 0);
            } else {
                prop_assert!(rows >= h);
            }
            prop_assert!(layout.bytes_per_sample() >= 1);
        }
    }

    #[test]
    fn yuv_coeffs_finite(m in 0u8..3) {
        let matrix = match m {
            0 => ColorMatrix::Bt601,
            1 => ColorMatrix::Bt709,
            _ => ColorMatrix::Bt2020,
        };
        let c = yuv_to_rgb_coeffs(matrix);
        for v in c {
            prop_assert!(v.is_finite());
        }
    }
}

#[test]
fn support_matrix_covers_http() {
    let m = support_matrix();
    assert!(m.iter().any(|s| s.scheme == StreamScheme::Http));
    assert!(m.iter().any(|s| s.scheme == StreamScheme::Https));
}

#[test]
fn hls_master_counts_variants() {
    let body = "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1280000\na.m3u8\n#EXT-X-STREAM-INF:BANDWIDTH=2560000\nb.m3u8\n";
    let p = parse_hls_manifest_body(body);
    assert!(p.is_master);
    assert_eq!(p.variant_count, 2);
}

#[test]
fn classify_opening_credits_is_intro() {
    assert_eq!(
        classify_chapter_title("Opening Credits"),
        ChapterClass::Intro
    );
}
