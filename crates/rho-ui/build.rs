//! On Windows, puts rho-ui's icon in `rho-ui.exe` as icon resource 1, the
//! one GPUI gives its windows and Explorer, the taskbar and the Start menu
//! show. It's drawn from `assets/icon.svg` here, so the SVG stays the one
//! source of the icon, as on Linux and in the macOS bundle.
use std::{env, fs, path::PathBuf};

/// The sizes Windows picks among, for 100% to 200% displays and the large
/// icon views; it scales the nearest for any other.
const SIZES: [u16; 7] = [16, 20, 24, 32, 40, 48, 256];

fn main() {
    let svg = "../../assets/icon.svg";
    println!("cargo:rerun-if-changed={svg}");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let out = PathBuf::from(env::var("OUT_DIR").expect("Cargo sets OUT_DIR"));
    let svg = fs::read(svg).expect("read assets/icon.svg");
    fs::write(out.join("rho-ui.ico"), ico(&svg)).expect("write rho-ui.ico");
    let rc = out.join("rho-ui.rc");
    fs::write(&rc, "1 ICON \"rho-ui.ico\"\n").expect("write rho-ui.rc");
    // "Required" fails the build when there's no resource compiler, rather
    // than leaving the icon out.
    embed_resource::compile(&rc, embed_resource::ParamsIncludeDirs([&out]))
        .manifest_required()
        .expect("compile rho-ui.rc");
}

/// An ICO file of `svg` drawn at each of [`SIZES`], each image a PNG, as
/// Windows reads since Vista.
fn ico(svg: &[u8]) -> Vec<u8> {
    let tree = resvg::usvg::Tree::from_data(svg, &resvg::usvg::Options::default())
        .expect("parse assets/icon.svg");
    let pngs: Vec<(u16, Vec<u8>)> = SIZES
        .iter()
        .map(|&size| {
            let side = u32::from(size);
            let mut pixmap = resvg::tiny_skia::Pixmap::new(side, side).expect("a nonzero size");
            let scale = f32::from(size) / tree.size().width();
            resvg::render(
                &tree,
                resvg::tiny_skia::Transform::from_scale(scale, scale),
                &mut pixmap.as_mut(),
            );
            (size, pixmap.encode_png().expect("encode a PNG"))
        })
        .collect();

    // The header, then a 16-byte entry for each image, then the images.
    let count = u16::try_from(pngs.len()).expect("a few sizes");
    let mut ico = Vec::new();
    ico.extend_from_slice(&[0, 0, 1, 0]);
    ico.extend_from_slice(&count.to_le_bytes());
    let mut offset = 6 + 16 * u32::from(count);
    for (size, png) in &pngs {
        // 0 means 256: the field is a byte.
        let side = u8::try_from(*size).unwrap_or(0);
        let len = u32::try_from(png.len()).expect("a small PNG");
        ico.extend_from_slice(&[side, side, 0, 0]);
        ico.extend_from_slice(&1u16.to_le_bytes()); // colour planes
        ico.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
        ico.extend_from_slice(&len.to_le_bytes());
        ico.extend_from_slice(&offset.to_le_bytes());
        offset += len;
    }
    for (_, png) in pngs {
        ico.extend_from_slice(&png);
    }
    ico
}
