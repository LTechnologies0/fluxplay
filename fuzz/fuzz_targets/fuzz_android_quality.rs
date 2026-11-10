#![no_main]
//! Soft-present budgets / present ladder used by Android iced + libmpv soft path.
use libfuzzer_sys::fuzz_target;

use fluxplay_player::fuzz_api::{AndroidDeviceCaps, AndroidPresentMode};

fuzz_target!(|data: &[u8]| {
    if data.len() < 20 {
        return;
    }
    let mut caps = AndroidDeviceCaps::default();
    caps.cores = (data[0] % 16).max(1) as u32;
    caps.refresh_hz = 30 + (data[1] as u32 % 120);
    caps.mediacodec_max_w = u32::from_le_bytes(data[2..6].try_into().unwrap()) % 7680;
    caps.mediacodec_max_h = u32::from_le_bytes(data[6..10].try_into().unwrap()) % 4320;
    caps.mediacodec_video = data[10] & 1 == 1;
    caps.mediacodec_4k = data[10] & 2 == 2;
    caps.mediacodec_hdr = data[10] & 4 == 4;
    caps.hdr_capable = data[10] & 8 == 8;
    caps.surface_ready = data[10] & 16 == 16;
    caps.sdk_int = 24 + (data[11] as u32 % 20);
    caps.panel_oled = data[12] & 1 == 1;
    caps.surface_w = u32::from_le_bytes(data[13..17].try_into().unwrap()) % 3840;
    caps.surface_h = (data[17] as u32).saturating_mul(8) % 2160;
    // Optional SoC hint string from remaining bytes (bounded).
    if let Ok(s) = std::str::from_utf8(&data[18..data.len().min(18 + 48)]) {
        caps.soc = s.chars().filter(|c| !c.is_control()).take(32).collect();
    }

    let _ = caps.compat_tier();
    let _ = caps.max_soft_wh();
    let budget = caps.soft_budget();
    let scale = budget.vf_scale();
    assert!(scale.starts_with("scale="));
    let ladder = caps.present_ladder();
    assert!(!ladder.is_empty());
    assert!(ladder.contains(&AndroidPresentMode::SoftRgba));
    let mode = caps.select_present_mode();
    assert!(ladder.contains(&mode));

    let fps = f32::from_bits(u32::from_le_bytes(data[0..4].try_into().unwrap()));
    let panel = caps.refresh_hz as f32;
    if fps.is_finite() && fps > 0.0 && panel > 0.0 {
        let snapped = AndroidDeviceCaps::snap_present_hz(fps, panel);
        assert!(snapped.is_finite());
        assert!(snapped > 0.0);
    }
});
