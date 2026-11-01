use std::sync::{Mutex, MutexGuard, PoisonError};

use serde::Deserialize;
use tauri::Manager;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub struct Rgb {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
}

impl Rgb {
    pub fn colorref(self) -> u32 {
        u32::from(self.red) | (u32::from(self.green) << 8) | (u32::from(self.blue) << 16)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scheme {
    System,
    Light,
    Dark,
}

impl Scheme {
    pub fn theme(self) -> Option<tauri::Theme> {
        match self {
            Scheme::System => None,
            Scheme::Light => Some(tauri::Theme::Light),
            Scheme::Dark => Some(tauri::Theme::Dark),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChromePalette {
    pub caption: Rgb,
    pub text: Rgb,
    pub border_focused: Rgb,
    pub border_unfocused: Rgb,
    pub scheme: Scheme,
}

impl ChromePalette {
    pub fn border(&self, focused: bool) -> Rgb {
        if focused {
            self.border_focused
        } else {
            self.border_unfocused
        }
    }
}

#[derive(Default)]
pub struct ChromeState(Mutex<Option<ChromePalette>>);

impl ChromeState {
    pub fn replace(&self, palette: ChromePalette) {
        *self.lock() = Some(palette);
    }

    pub fn current(&self) -> Option<ChromePalette> {
        *self.lock()
    }

    fn lock(&self) -> MutexGuard<'_, Option<ChromePalette>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

pub fn apply(window: &tauri::WebviewWindow, palette: ChromePalette, focused: bool) {
    #[cfg(windows)]
    frame::apply(window, palette, focused);
    #[cfg(not(windows))]
    let _ = (window, palette, focused);
}

pub fn on_focus(app: &tauri::AppHandle, label: &str, focused: bool) {
    let Some(palette) = app.state::<ChromeState>().current() else {
        return;
    };
    let Some(window) = app.get_webview_window(label) else {
        return;
    };
    #[cfg(windows)]
    frame::border(&window, palette.border(focused));
    #[cfg(not(windows))]
    let _ = (window, palette, focused);
}

#[cfg(windows)]
mod frame {
    use std::sync::Once;

    use windows_sys::Win32::Graphics::Dwm::{
        DwmSetWindowAttribute, DWMWA_BORDER_COLOR, DWMWA_CAPTION_COLOR, DWMWA_TEXT_COLOR,
    };

    use super::{ChromePalette, Rgb};

    static UNSUPPORTED: Once = Once::new();

    pub fn apply(window: &tauri::WebviewWindow, palette: ChromePalette, focused: bool) {
        if let Err(error) = window.set_theme(palette.scheme.theme()) {
            tracing::debug!(%error, "window theme not applied");
        }
        set(window, DWMWA_CAPTION_COLOR, palette.caption);
        set(window, DWMWA_TEXT_COLOR, palette.text);
        border(window, palette.border(focused));
    }

    pub fn border(window: &tauri::WebviewWindow, color: Rgb) {
        set(window, DWMWA_BORDER_COLOR, color);
    }

    fn set(window: &tauri::WebviewWindow, attribute: i32, color: Rgb) {
        let Ok(hwnd) = window.hwnd() else {
            return;
        };
        let value: u32 = color.colorref();
        let result = unsafe {
            DwmSetWindowAttribute(
                hwnd.0,
                attribute as u32,
                std::ptr::from_ref(&value).cast(),
                std::mem::size_of::<u32>() as u32,
            )
        };
        if result < 0 {
            UNSUPPORTED.call_once(|| {
                tracing::debug!(
                    attribute,
                    result,
                    "window frame color not applied; the OS default frame stays"
                );
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EMBER: Rgb = Rgb {
        red: 0xba,
        green: 0x55,
        blue: 0x18,
    };
    const HAIRLINE: Rgb = Rgb {
        red: 0xc9,
        green: 0xc4,
        blue: 0xb8,
    };

    fn palette() -> ChromePalette {
        serde_json::from_value(serde_json::json!({
            "caption": { "red": 233, "green": 230, "blue": 222 },
            "text": { "red": 36, "green": 39, "blue": 44 },
            "borderFocused": { "red": 186, "green": 85, "blue": 24 },
            "borderUnfocused": { "red": 201, "green": 196, "blue": 184 },
            "scheme": "dark"
        }))
        .expect("the payload themeStore sends deserializes")
    }

    #[test]
    fn colorref_packs_blue_high_and_red_low() {
        assert_eq!(EMBER.colorref(), 0x0018_55ba);
        assert_eq!(
            Rgb {
                red: 255,
                green: 0,
                blue: 0
            }
            .colorref(),
            0x0000_00ff
        );
        assert_eq!(
            Rgb {
                red: 0,
                green: 0,
                blue: 255
            }
            .colorref(),
            0x00ff_0000
        );
    }

    #[test]
    fn colorref_never_reaches_the_dwm_sentinels() {
        let white = Rgb {
            red: 255,
            green: 255,
            blue: 255,
        };
        assert_eq!(white.colorref(), 0x00ff_ffff);
        assert!(white.colorref() < 0xffff_fffe);
    }

    #[test]
    fn frontend_payload_maps_onto_the_palette() {
        let palette = palette();
        assert_eq!(palette.border_focused, EMBER);
        assert_eq!(palette.border_unfocused, HAIRLINE);
        assert_eq!(palette.scheme, Scheme::Dark);
    }

    #[test]
    fn border_is_ember_only_while_focused() {
        let palette = palette();
        assert_eq!(palette.border(true), EMBER);
        assert_eq!(palette.border(false), HAIRLINE);
    }

    #[test]
    fn system_scheme_leaves_the_frame_following_the_os() {
        assert_eq!(Scheme::System.theme(), None);
        assert_eq!(Scheme::Light.theme(), Some(tauri::Theme::Light));
        assert_eq!(Scheme::Dark.theme(), Some(tauri::Theme::Dark));
    }

    #[test]
    fn out_of_range_channels_are_rejected() {
        let parsed: Result<Rgb, _> =
            serde_json::from_value(serde_json::json!({ "red": 256, "green": 0, "blue": 0 }));
        assert!(parsed.is_err());
    }

    #[test]
    fn state_remembers_the_latest_palette_for_focus_changes() {
        let state = ChromeState::default();
        assert_eq!(state.current(), None);
        state.replace(palette());
        assert_eq!(state.current(), Some(palette()));
    }
}
