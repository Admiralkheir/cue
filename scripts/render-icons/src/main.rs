//! Renders `assets/icon.svg` into the icon files the app ships with:
//!
//! - `assets/icon.png`: 1024x1024, for the README and anything else that wants a big image
//! - `assets/cue.png`: 512x512, the Linux desktop icon
//! - `assets/cue.ico`: embedded in `cue.exe` (window, taskbar and Explorer icon)
//! - `assets/Cue.icns`: the macOS bundle icon, with the tile shrunk to Apple's icon grid
//!
//! Run from the repository root after changing the SVG:
//!
//!     cargo run --release --manifest-path scripts/render-icons/Cargo.toml

use std::fs::File;
use std::io::BufWriter;
use std::path::Path;

use anyhow::{Context, Result};
use image::codecs::ico::{IcoEncoder, IcoFrame};
use image::{ExtendedColorType, RgbaImage};
use resvg::{tiny_skia, usvg};

/// The tile in the SVG spans 64..960 on a 1024 canvas.
const TILE_START: f32 = 64.0;
const TILE_SIZE: f32 = 896.0;
/// Apple's icon grid: an 824px tile centered on the 1024px canvas.
const MAC_TILE_START: f32 = 100.0;
const MAC_TILE_SIZE: f32 = 824.0;

const ICO_SIZES: [u32; 8] = [16, 20, 24, 32, 40, 48, 64, 256];

fn main() -> Result<()> {
    let assets = Path::new("assets");
    let svg =
        std::fs::read(assets.join("icon.svg")).context("run this from the repository root")?;
    let tree = usvg::Tree::from_data(&svg, &usvg::Options::default())?;

    render(&tree, 1024, false)?.save(assets.join("icon.png"))?;
    render(&tree, 512, false)?.save(assets.join("cue.png"))?;

    let frames = ICO_SIZES
        .iter()
        .map(|&size| {
            let image = render(&tree, size, false)?;
            Ok(IcoFrame::as_png(
                image.as_raw(),
                size,
                size,
                ExtendedColorType::Rgba8,
            )?)
        })
        .collect::<Result<Vec<_>>>()?;
    let ico = BufWriter::new(File::create(assets.join("cue.ico"))?);
    IcoEncoder::new(ico).encode_images(&frames)?;

    let mut family = icns::IconFamily::new();
    for (size, kind) in [
        (16, icns::IconType::RGBA32_16x16),
        (32, icns::IconType::RGBA32_16x16_2x),
        (32, icns::IconType::RGBA32_32x32),
        (64, icns::IconType::RGBA32_32x32_2x),
        (128, icns::IconType::RGBA32_128x128),
        (256, icns::IconType::RGBA32_128x128_2x),
        (256, icns::IconType::RGBA32_256x256),
        (512, icns::IconType::RGBA32_256x256_2x),
        (512, icns::IconType::RGBA32_512x512),
        (1024, icns::IconType::RGBA32_512x512_2x),
    ] {
        let image = render(&tree, size, true)?;
        let image = icns::Image::from_data(icns::PixelFormat::RGBA, size, size, image.into_raw())?;
        family.add_icon_with_type(&image, kind)?;
    }
    family.write(BufWriter::new(File::create(assets.join("Cue.icns"))?))?;

    println!("wrote assets/icon.png, assets/cue.png, assets/cue.ico and assets/Cue.icns");
    Ok(())
}

/// Renders the SVG at `size` x `size`. With `mac_grid` the tile is scaled down to Apple's
/// icon grid, so the icon matches the size of other apps in the Dock.
fn render(tree: &usvg::Tree, size: u32, mac_grid: bool) -> Result<RgbaImage> {
    let mut pixmap = tiny_skia::Pixmap::new(size, size).context("empty icon size")?;
    let canvas = size as f32 / 1024.0;
    let transform = if mac_grid {
        let scale = MAC_TILE_SIZE / TILE_SIZE;
        let offset = MAC_TILE_START - TILE_START * scale;
        tiny_skia::Transform::from_row(scale, 0.0, 0.0, scale, offset, offset)
            .post_scale(canvas, canvas)
    } else {
        tiny_skia::Transform::from_scale(canvas, canvas)
    };
    resvg::render(tree, transform, &mut pixmap.as_mut());

    // tiny-skia stores premultiplied alpha; PNG, ICO and ICNS want it straight.
    let mut rgba = Vec::with_capacity((size * size * 4) as usize);
    for pixel in pixmap.pixels() {
        let c = pixel.demultiply();
        rgba.extend_from_slice(&[c.red(), c.green(), c.blue(), c.alpha()]);
    }
    RgbaImage::from_raw(size, size, rgba).context("pixel buffer size")
}
