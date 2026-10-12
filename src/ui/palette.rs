//! Палитры по роли данных; цвет не заменяет подписи и числовые таблицы.
use eframe::egui::{self, Color32};

pub(super) fn surface(dark: bool) -> Color32 {
    if dark { rgb(0x1a1a19) } else { rgb(0xfcfcfb) }
}

pub(super) fn ink(dark: bool) -> Color32 {
    if dark { rgb(0xf0efec) } else { rgb(0x0b0b0b) }
}

pub(super) fn grid(dark: bool) -> Color32 {
    if dark { rgb(0x383835) } else { rgb(0xe1e0d9) }
}

pub(super) fn series(slot: usize, dark: bool) -> Color32 {
    let colors = if dark {
        [0x3987e5, 0xd95926, 0x199e70]
    } else {
        [0x2a78d6, 0xeb6834, 0x1baf7a]
    };
    colors.get(slot).map_or_else(|| ink(dark), |&c| rgb(c))
}

fn rgb(value: u32) -> Color32 {
    Color32::from_rgb((value >> 16) as u8, (value >> 8) as u8, value as u8)
}

fn mix(a: Color32, b: Color32, t: f64) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let c = |a: u8, b: u8| (f64::from(a) * (1.0 - t) + f64::from(b) * t).round() as u8;
    Color32::from_rgb(c(a.r(), b.r()), c(a.g(), b.g()), c(a.b(), b.b()))
}

/// Насыщение ограничивает только изображение, а не вычисляемое поле.
pub(super) fn diverging(normalized: f64, dark: bool) -> Color32 {
    let zero = if dark { rgb(0x383835) } else { rgb(0xf0efec) };
    let pole = if normalized < 0.0 {
        if dark { rgb(0x3987e5) } else { rgb(0x2a78d6) }
    } else if dark {
        rgb(0xe66767)
    } else {
        rgb(0xe34948)
    };
    mix(zero, pole, normalized.abs())
}

pub(super) fn sequential(normalized: f64, dark: bool) -> Color32 {
    let (low, high) = if dark {
        (0x104281, 0xcde2fb)
    } else {
        (0xcde2fb, 0x0d366b)
    };
    mix(rgb(low), rgb(high), normalized)
}

pub(super) fn configure(ctx: &egui::Context, dark: bool) {
    ctx.set_theme(if dark {
        egui::Theme::Dark
    } else {
        egui::Theme::Light
    });
    let mut visuals = if dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    visuals.panel_fill = surface(dark);
    visuals.window_fill = surface(dark);
    visuals.extreme_bg_color = if dark { rgb(0x0d0d0d) } else { rgb(0xf9f9f7) };
    visuals.override_text_color = Some(ink(dark));
    visuals.selection.bg_fill = series(0, dark).gamma_multiply(0.28);
    visuals.selection.stroke = egui::Stroke::new(1.5, series(0, dark));
    ctx.set_visuals(visuals);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lightness(c: Color32) -> f64 {
        let linear = |v: u8| {
            let v = f64::from(v) / 255.0;
            if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        let (r, g, b) = (linear(c.r()), linear(c.g()), linear(c.b()));
        let l = (0.412_221_470_8 * r + 0.536_332_536_3 * g + 0.051_445_992_9 * b).cbrt();
        let m = (0.211_903_498_2 * r + 0.680_699_545_1 * g + 0.107_396_956_6 * b).cbrt();
        let s = (0.088_302_461_9 * r + 0.281_718_837_6 * g + 0.629_978_700_5 * b).cbrt();
        0.210_454_255_3 * l + 0.793_617_785 * m - 0.004_072_046_8 * s
    }

    #[test]
    fn magnitude_and_both_polarity_arms_have_monotone_lightness() {
        for dark in [false, true] {
            for ramp in [0, 1, 2] {
                let sample = |i: usize| {
                    let t = i as f64 / 64.0;
                    lightness(match ramp {
                        0 => sequential(t, dark),
                        1 => diverging(t, dark),
                        _ => diverging(-t, dark),
                    })
                };
                for i in 1..=64 {
                    let difference = sample(i) - sample(i - 1);
                    assert!(if dark {
                        difference >= 0.0
                    } else {
                        difference <= 0.0
                    });
                }
            }
        }
    }

    #[test]
    fn polarity_has_neutral_zero_and_display_only_saturation() {
        for dark in [false, true] {
            let zero = diverging(0.0, dark);
            assert!((i16::from(zero.r()) - i16::from(zero.g())).abs() <= 1);
            assert_eq!(diverging(20.0, dark), diverging(1.0, dark));
            assert_eq!(sequential(20.0, dark), sequential(1.0, dark));
        }
    }
}
