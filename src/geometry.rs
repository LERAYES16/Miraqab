//! Sizes, rectangles and the aspect-ratio-preserving fit used by the
//! fixed-output-resolution mode ("Mode A").

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Size {
    pub w: u32,
    pub h: u32,
}

impl Size {
    pub fn new(w: u32, h: u32) -> Size {
        Size { w, h }
    }
    pub fn is_empty(&self) -> bool {
        self.w == 0 || self.h == 0
    }
    /// Round both sides DOWN to even numbers (required by yuv420p), min 2.
    pub fn even(&self) -> Size {
        Size { w: (self.w & !1).max(2), h: (self.h & !1).max(2) }
    }
}

impl std::fmt::Display for Size {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}x{}", self.w, self.h)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    pub fn size(&self) -> Size {
        Size { w: self.w, h: self.h }
    }
}

/// How a source image is placed inside the output canvas.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placement {
    /// Size of the image after scaling (even numbers, never larger than the canvas).
    pub scaled: Size,
    /// Offset of the scaled image inside the canvas (even numbers).
    pub pad_x: u32,
    pub pad_y: u32,
    /// False when the source already has exactly the scaled size: no rescale is emitted.
    pub needs_scale: bool,
    /// False when the scaled image fills the canvas completely: no padding is emitted.
    pub needs_pad: bool,
}

/// Fit `src` inside `canvas`, preserving the aspect ratio (letterbox / pillarbox).
/// The picture is never stretched. Integer arithmetic only, so results are exact
/// and reproducible. `canvas` must have even, non-zero sides (see [`Size::even`]).
pub fn fit(src: Size, canvas: Size) -> Placement {
    assert!(!canvas.is_empty() && canvas.w % 2 == 0 && canvas.h % 2 == 0, "canvas must be even");
    if src.is_empty() {
        return Placement { scaled: canvas, pad_x: 0, pad_y: 0, needs_scale: false, needs_pad: false };
    }
    let (sw, sh) = (src.w as u64, src.h as u64);
    let (cw, ch) = (canvas.w as u64, canvas.h as u64);
    // Compare aspect ratios without division: sw/sh <= cw/ch  <=>  sw*ch <= cw*sh
    let (mut w, mut h) = if sw * ch <= cw * sh {
        // height-limited (pillarbox when strictly less)
        (((sw * ch) + sh / 2) / sh, ch)
    } else {
        // width-limited (letterbox)
        (cw, ((sh * cw) + sw / 2) / sw)
    };
    w = (w & !1).clamp(2, cw);
    h = (h & !1).clamp(2, ch);
    let scaled = Size { w: w as u32, h: h as u32 };
    let pad_x = (((cw - w) / 2) & !1) as u32;
    let pad_y = (((ch - h) / 2) & !1) as u32;
    Placement {
        scaled,
        pad_x,
        pad_y,
        needs_scale: scaled != src,
        needs_pad: scaled != canvas,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_size_needs_nothing() {
        let p = fit(Size::new(1280, 720), Size::new(1280, 720));
        assert!(!p.needs_scale && !p.needs_pad);
        assert_eq!(p.scaled, Size::new(1280, 720));
    }

    #[test]
    fn spec_example_640x480_then_1920x1080_in_1280x720() {
        // 640x480 (4:3) in a 16:9 canvas: pillarbox
        let a = fit(Size::new(640, 480), Size::new(1280, 720));
        assert_eq!(a.scaled, Size::new(960, 720));
        assert_eq!((a.pad_x, a.pad_y), (160, 0));
        assert!(a.needs_scale && a.needs_pad);
        // maximised to 1920x1080 (16:9): fills the canvas, no bars
        let b = fit(Size::new(1920, 1080), Size::new(1280, 720));
        assert_eq!(b.scaled, Size::new(1280, 720));
        assert!(b.needs_scale && !b.needs_pad);
    }

    #[test]
    fn taller_source_is_pillarboxed_wider_is_letterboxed() {
        let tall = fit(Size::new(1920, 1200), Size::new(1280, 720)); // 16:10
        assert_eq!(tall.scaled, Size::new(1152, 720));
        assert_eq!(tall.pad_x, 64);
        let wide = fit(Size::new(2560, 1080), Size::new(1280, 720)); // 21:9
        assert_eq!(wide.scaled.w, 1280);
        assert!(wide.scaled.h < 720 && wide.pad_y > 0);
    }

    #[test]
    fn never_stretches() {
        for (sw, sh) in [(640, 480), (801, 333), (333, 801), (1920, 1080), (100, 100), (3840, 2160)] {
            let canvas = Size::new(1280, 720);
            let p = fit(Size::new(sw, sh), canvas);
            // scaled aspect equals source aspect within rounding to an even pixel
            let a = p.scaled.w as f64 / p.scaled.h as f64;
            let b = sw as f64 / sh as f64;
            assert!((a - b).abs() / b < 0.02, "{}x{} -> {} aspect {} vs {}", sw, sh, p.scaled, a, b);
            assert!(p.scaled.w <= canvas.w && p.scaled.h <= canvas.h);
            assert_eq!(p.pad_x % 2, 0);
            assert_eq!(p.pad_y % 2, 0);
        }
    }

    #[test]
    fn small_source_is_enlarged_not_left_tiny() {
        let p = fit(Size::new(320, 240), Size::new(1280, 720));
        assert_eq!(p.scaled, Size::new(960, 720));
    }

    #[test]
    fn odd_source_dimensions_are_handled() {
        let p = fit(Size::new(641, 481), Size::new(640, 480));
        assert_eq!(p.scaled.w % 2, 0);
        assert_eq!(p.scaled.h % 2, 0);
        assert!(p.scaled.w <= 640 && p.scaled.h <= 480);
    }

    #[test]
    fn even_rounds_down() {
        assert_eq!(Size::new(641, 481).even(), Size::new(640, 480));
        assert_eq!(Size::new(1, 1).even(), Size::new(2, 2));
    }
}
