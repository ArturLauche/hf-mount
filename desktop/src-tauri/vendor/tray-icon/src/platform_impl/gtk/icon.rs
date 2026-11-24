// Copyright 2022-2022 Tauri Programme within The Commons Conservancy
// SPDX-License-Identifier: Apache-2.0
// SPDX-License-Identifier: MIT

use std::{fs::File, io::BufWriter, path::Path};

use crate::icon::{BadIcon, RgbaIcon};

#[derive(Debug, Clone)]
pub struct PlatformIcon(RgbaIcon);

impl PlatformIcon {
    pub fn from_rgba(rgba: Vec<u8>, width: u32, height: u32) -> Result<Self, BadIcon> {
        RgbaIcon::from_rgba(rgba, width, height).map(Self)
    }

    pub fn write_to_png(&self, path: impl AsRef<Path>) -> crate::Result<()> {
        let png = File::create(path)?;
        let w = &mut BufWriter::new(png);

        let mut encoder = png::Encoder::new(w, self.0.width, self.0.height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);

        let mut writer = encoder.write_header()?;
        writer.write_image_data(&self.0.rgba)?;
        writer.finish()?;

        Ok(())
    }
}

#[test]
fn validates_rgba_before_using_the_icon() {
    assert!(matches!(
        PlatformIcon::from_rgba(vec![1, 2, 3], 1, 1),
        Err(BadIcon::ByteCountNotDivisibleBy4 { .. })
    ));
    assert!(matches!(
        PlatformIcon::from_rgba(vec![], 65536, 65536),
        Err(BadIcon::DimensionsVsPixelCount { .. })
    ));
    assert!(PlatformIcon::from_rgba(vec![1, 2, 3, 255], 1, 1).is_ok());
}
