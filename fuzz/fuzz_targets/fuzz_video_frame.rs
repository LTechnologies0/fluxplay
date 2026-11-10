#![no_main]
//! Pixel layout / YUV coeff math used by soft FFmpeg+libmpv → iced path.
use libfuzzer_sys::fuzz_target;

use fluxplay_player::fuzz_api::{yuv_to_rgb_coeffs, ColorMatrix, PixelLayout};

fuzz_target!(|data: &[u8]| {
    if data.len() < 5 {
        return;
    }
    let h = u32::from_le_bytes(data[0..4].try_into().unwrap()) % 8192;
    let layout = match data[4] % 3 {
        0 => PixelLayout::Rgba,
        1 => PixelLayout::Nv12,
        _ => PixelLayout::P010,
    };
    let rows = layout.rows(h);
    if h == 0 {
        assert_eq!(rows, 0);
    } else {
        assert!(rows >= h);
    }
    assert!(layout.bytes_per_sample() >= 1);

    let matrix = match data[4] % 3 {
        0 => ColorMatrix::Bt601,
        1 => ColorMatrix::Bt709,
        _ => ColorMatrix::Bt2020,
    };
    for v in yuv_to_rgb_coeffs(matrix) {
        assert!(v.is_finite());
    }
});
