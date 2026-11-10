//! Pending media keys from MediaSession / notification (JNI → iced).

use std::sync::atomic::{AtomicU8, Ordering};

const NONE: u8 = 0;
const PLAY_PAUSE: u8 = 1;
const PLAY: u8 = 2;
const PAUSE: u8 = 3;
const STOP: u8 = 4;

static PENDING: AtomicU8 = AtomicU8::new(NONE);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaAction {
    TogglePause,
    Play,
    Pause,
    Stop,
}

pub fn on_media_action(code: i32) {
    let v = match code {
        1 => PLAY,
        2 => PAUSE,
        3 => STOP,
        4 => PLAY_PAUSE,
        _ => NONE,
    };
    if v != NONE {
        PENDING.store(v, Ordering::SeqCst);
    }
}

pub fn take_pending() -> Option<MediaAction> {
    let v = PENDING.swap(NONE, Ordering::SeqCst);
    match v {
        PLAY_PAUSE => Some(MediaAction::TogglePause),
        PLAY => Some(MediaAction::Play),
        PAUSE => Some(MediaAction::Pause),
        STOP => Some(MediaAction::Stop),
        _ => None,
    }
}
