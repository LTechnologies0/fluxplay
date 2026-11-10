#![no_main]
use libfuzzer_sys::fuzz_target;

use fluxplay::fuzz_api::{
    assert_viewport_sane, BrowseTile, BrowseTileCache, BROWSE_TILE_CACHE_CAP,
};

fuzz_target!(|data: &[u8]| {
    if data.is_empty() {
        return;
    }
    let mut cache = BrowseTileCache::default();
    let mut i = 0usize;
    // Interpret bytes as a sequence of store ops: [opcode, start_lo, n_lo, ...]
    while i + 2 < data.len() {
        let op = data[i];
        let start = data[i + 1] as usize;
        let n = (data[i + 2] as usize % 64) + 1;
        i += 3;
        match op % 3 {
            0 => {
                let tiles: Vec<_> = (0..n)
                    .map(|k| BrowseTile {
                        id: format!("{k}"),
                        name: if data.get(i).copied().unwrap_or(1) == 0 {
                            String::new()
                        } else {
                            format!("t{k}")
                        },
                        art_url: None,
                        category_id: None,
                        source_id: None,
                        year: None,
                        genre: None,
                        rating: None,
                        group: None,
                    })
                    .collect();
                cache.store_contiguous(start, tiles);
            }
            1 => {
                let pairs: Vec<_> = (0..n)
                    .map(|k| {
                        (
                            start + k * 2,
                            BrowseTile {
                                id: format!("p{k}"),
                                name: "x".into(),
                                art_url: None,
                                category_id: None,
                                source_id: None,
                                year: None,
                                genre: None,
                                rating: None,
                                group: None,
                            },
                        )
                    })
                    .collect();
                cache.store_at(pairs);
            }
            _ => {
                let end = start.saturating_add(n);
                let h = cache.viewport_health(start, end);
                let _ = assert_viewport_sane(h.window_len, h.filled, h.holes, h.unpaintable);
                let _ = cache.first_unfilled_run(start, end);
            }
        }
        assert!(cache.len() <= BROWSE_TILE_CACHE_CAP);
    }
});
