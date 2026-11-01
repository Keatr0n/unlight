use crate::{
    error::{Error, Result},
    mask::MaskRaster,
};
use fast_image_resize::{FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer, images};

/// Long edge of the image a selection works on. A mask finer than this adds nothing the
/// feathered edges of a local adjustment could show.
pub const WORKING_EDGE: u32 = 2048;

/// The size to render a photo at for selection, keeping its shape.
pub fn working_size(width: u32, height: u32) -> [u32; 2] {
    let long_edge = width.max(height).max(1);
    let scale = f64::from(WORKING_EDGE.min(long_edge)) / f64::from(long_edge);
    [width, height].map(|side| ((f64::from(side) * scale).round() as u32).max(1))
}

/// A gamma-encoded 8-bit picture, as a rendered preview provides.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RgbImage {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

impl RgbImage {
    pub fn from_rgba(width: u32, height: u32, rgba: &[u8]) -> Self {
        let pixels = rgba.as_chunks::<4>().0.iter().flat_map(|&[red, green, blue, _]| [red, green, blue]).collect();
        Self { width, height, pixels }
    }

    pub(crate) fn resized(&self, width: u32, height: u32) -> Result<Self> {
        let pixels = resize(&self.pixels, [self.width, self.height], [width, height], PixelType::U8x3, FilterType::Bilinear)?;
        Ok(Self { width, height, pixels })
    }
}

pub(crate) fn resize_coverage(values: &[f32], from: [u32; 2], to: [u32; 2]) -> Result<Vec<f32>> {
    let bytes = resize(bytemuck::cast_slice(values), from, to, PixelType::F32, FilterType::CatmullRom)?;
    Ok(bytemuck::pod_collect_to_vec(&bytes))
}

fn resize(data: &[u8], from: [u32; 2], to: [u32; 2], pixel: PixelType, filter: FilterType) -> Result<Vec<u8>> {
    let failed = |error: &dyn std::fmt::Display| Error::Resize(error.to_string());
    let source = images::ImageRef::new(from[0], from[1], data, pixel).map_err(|error| failed(&error))?;
    let mut target = images::Image::new(to[0], to[1], pixel);
    let options = ResizeOptions::new().resize_alg(ResizeAlg::Convolution(filter));
    Resizer::new().resize(&source, &mut target, &options).map_err(|error| failed(&error))?;
    Ok(target.into_vec())
}

/// How a sensor image turns to stand upright. The engine works in sensor orientation, but the
/// models need to see the sky at the top.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Turn {
    #[default]
    None,
    Half,
    Clockwise,
    CounterClockwise,
}

impl Turn {
    /// From LibRaw's flip code.
    pub fn from_libraw(flip: i32) -> Self {
        match flip {
            3 => Self::Half,
            5 => Self::CounterClockwise,
            6 => Self::Clockwise,
            _ => Self::None,
        }
    }

    fn inverse(self) -> Self {
        match self {
            Self::Clockwise => Self::CounterClockwise,
            Self::CounterClockwise => Self::Clockwise,
            other => other,
        }
    }

    pub(crate) fn upright(self, image: &RgbImage) -> RgbImage {
        let [width, height] = self.size(image.width, image.height);
        let pixels = self.apply::<3, u8>(&image.pixels, image.width, image.height);
        RgbImage { width, height, pixels }
    }

    /// Takes a mask made on the upright image back to the sensor orientation of the photo.
    pub(crate) fn to_sensor(self, mask: &MaskRaster) -> MaskRaster {
        let inverse = self.inverse();
        let [width, height] = inverse.size(mask.width, mask.height);
        let values = inverse.apply::<1, u16>(&mask.values, mask.width, mask.height);
        MaskRaster { width, height, values }
    }

    fn size(self, width: u32, height: u32) -> [u32; 2] {
        match self {
            Self::None | Self::Half => [width, height],
            Self::Clockwise | Self::CounterClockwise => [height, width],
        }
    }

    fn apply<const CHANNELS: usize, T: Copy>(self, pixels: &[T], width: u32, height: u32) -> Vec<T> {
        let (w, h) = (width as usize, height as usize);
        let out_w = self.size(width, height)[0] as usize;
        let mut turned = pixels.to_vec();
        for y in 0..h {
            for x in 0..w {
                let (to_x, to_y) = match self {
                    Self::None => (x, y),
                    Self::Half => (w - 1 - x, h - 1 - y),
                    Self::Clockwise => (h - 1 - y, x),
                    Self::CounterClockwise => (y, w - 1 - x),
                };
                let from = (y * w + x) * CHANNELS;
                let to = (to_y * out_w + to_x) * CHANNELS;
                turned[to..to + CHANNELS].copy_from_slice(&pixels[from..from + CHANNELS]);
            }
        }
        turned
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn numbered(width: u32, height: u32) -> MaskRaster {
        MaskRaster { width, height, values: (0..(width * height) as u16).collect() }
    }

    #[test]
    fn working_size_keeps_the_shape_and_never_upscales() {
        assert_eq!(working_size(6000, 4000), [2048, 1365]);
        assert_eq!(working_size(4000, 6000), [1365, 2048]);
        assert_eq!(working_size(800, 600), [800, 600]);
    }

    #[test]
    fn every_turn_undoes_itself() {
        for flip in [0, 3, 5, 6] {
            let turn = Turn::from_libraw(flip);
            let mask = numbered(4, 3);
            let upright = RgbImage { width: 4, height: 3, pixels: vec![0; 36] };
            let standing = turn.upright(&upright);
            let as_shown = MaskRaster {
                width: standing.width,
                height: standing.height,
                values: turn.apply::<1, u16>(&mask.values, 4, 3),
            };
            assert_eq!(turn.to_sensor(&as_shown), mask, "flip {flip}");
        }
    }

    #[test]
    fn a_clockwise_turn_puts_the_left_edge_on_top() {
        let mut image = RgbImage { width: 2, height: 1, pixels: vec![10, 11, 12, 20, 21, 22] };
        image = Turn::Clockwise.upright(&image);
        assert_eq!((image.width, image.height), (1, 2));
        assert_eq!(image.pixels, [10, 11, 12, 20, 21, 22]);
    }

    #[test]
    fn the_turn_matches_the_cli_orientation_for_flip_six() {
        // (x, y) -> (height - 1 - y, x), as `unlight-cli render` applies it.
        let image = RgbImage { width: 2, height: 2, pixels: vec![1, 0, 0, 2, 0, 0, 3, 0, 0, 4, 0, 0] };
        let turned = Turn::from_libraw(6).upright(&image);
        assert_eq!(turned.pixels, [3, 0, 0, 1, 0, 0, 4, 0, 0, 2, 0, 0]);
    }

    #[test]
    fn resizing_coverage_keeps_a_flat_field_flat() {
        let resized = resize_coverage(&[0.25; 16], [4, 4], [9, 7]).unwrap();
        assert_eq!(resized.len(), 63);
        assert!(resized.iter().all(|value| (value - 0.25).abs() < 1e-5));
    }
}
