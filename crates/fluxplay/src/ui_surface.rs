#![allow(dead_code)]
//! Form-factor surface — drives layout / focus / touch targets across OS UIs.
//!
//! Width-only breakpoints cannot tell a 4K desktop from Android TV. This layer
//! folds OS signals (leanback) with window geometry into one paint contract.

use crate::theme::Breakpoint;

/// Physical / product surface the UI is designed for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiSurface {
    /// Mouse + keyboard (Linux / Windows desktop shell).
    Desktop,
    /// Touch phone (portrait or compact).
    Phone,
    /// Touch tablet / fold medium.
    Tablet,
    /// 10-foot leanback (Android TV / living-room remote).
    Television,
}

impl UiSurface {
    /// Detect from OS + content size (logical px after insets).
    ///
    /// Override: `FLUXPLAY_FORCE_TV=1` (desktop couch / CI) forces [`UiSurface::Television`].
    pub fn detect(content_w: f32, content_h: f32) -> Self {
        if std::env::var_os("FLUXPLAY_FORCE_TV").is_some() {
            return Self::Television;
        }
        #[cfg(target_os = "android")]
        {
            if crate::android_bridge::is_television() {
                return Self::Television;
            }
        }
        let landscape = content_w > content_h * 1.05;
        let compact =
            content_w.min(content_h) < 520.0 || content_h < 480.0 || (landscape && content_h < 560.0);
        if compact {
            return Self::Phone;
        }
        match Breakpoint::from_width(content_w) {
            Breakpoint::Phone => Self::Phone,
            Breakpoint::Tablet => Self::Tablet,
            // Wide windows are desktop shells; leanback is the only Television path
            // (or FLUXPLAY_FORCE_TV / Android isTelevision).
            Breakpoint::Laptop | Breakpoint::Desktop | Breakpoint::Tv => Self::Desktop,
        }
    }

    pub fn is_television(self) -> bool {
        matches!(self, Self::Television)
    }

    pub fn is_touch(self) -> bool {
        matches!(self, Self::Phone | Self::Tablet)
    }

    /// Prefer D-pad / arrow focus chrome (TV + desktop couch keyboard).
    pub fn wants_focus_chrome(self) -> bool {
        matches!(self, Self::Television | Self::Desktop)
    }

    /// Minimum interactive target (M3 phone 48 / TV ~64).
    pub fn touch_target(self) -> f32 {
        match self {
            Self::Television => 64.0,
            Self::Tablet => 52.0,
            Self::Phone => 48.0,
            Self::Desktop => 40.0,
        }
    }

    /// Extra outer pad for TV overscan / safe area.
    pub fn overscan_pad(self) -> f32 {
        match self {
            Self::Television => 24.0,
            _ => 0.0,
        }
    }

    /// Type / mosaic scale bump for 10-foot viewing.
    pub fn type_scale(self) -> f32 {
        match self {
            Self::Television => 1.25,
            Self::Tablet => 1.05,
            _ => 1.0,
        }
    }

    /// Force layout breakpoint used by [`crate::theme::LayoutMetrics`].
    pub fn layout_breakpoint(self, content_w: f32, content_h: f32) -> Breakpoint {
        if self.is_television() {
            return Breakpoint::Tv;
        }
        let landscape = content_w > content_h * 1.05;
        let compact =
            content_w.min(content_h) < 520.0 || content_h < 480.0 || (landscape && content_h < 560.0);
        if compact || matches!(self, Self::Phone) {
            Breakpoint::Phone
        } else if matches!(self, Self::Tablet) {
            Breakpoint::Tablet
        } else {
            Breakpoint::from_width(content_w.max(280.0))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phone_from_compact_geometry() {
        assert_eq!(UiSurface::detect(360.0, 720.0), UiSurface::Phone);
    }

    #[test]
    fn desktop_from_wide_window() {
        assert_eq!(UiSurface::detect(1440.0, 900.0), UiSurface::Desktop);
    }

    #[test]
    fn television_touch_target_is_ten_foot() {
        assert!(UiSurface::Television.touch_target() >= 64.0);
        assert!(UiSurface::Television.overscan_pad() >= 24.0);
        assert!(UiSurface::Television.wants_focus_chrome());
        let m = crate::theme::LayoutMetrics::compute_for(UiSurface::Television, 1920.0, 1080.0);
        assert_eq!(m.bp, Breakpoint::Tv);
        assert!(m.focus_chrome);
        assert!(m.pad >= 24.0);
        assert!(m.cols <= 6);
    }
}
