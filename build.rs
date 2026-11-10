use std::{fs, io, path::PathBuf};

// Generate a self-contained multi-size Windows .ico at compile time.
// The artwork is a database cylinder with a connection accent.
fn icon_bmp(size: usize) -> Vec<u8> {
    let mut data = Vec::new();
    let header_size = 40u32;
    let pixel_bytes = (size * size * 4) as u32;
    let mask_stride = ((size + 31) / 32) * 4;
    let mask_bytes = (mask_stride * size) as u32;
    data.extend(header_size.to_le_bytes());
    data.extend((size as i32).to_le_bytes());
    data.extend(((size * 2) as i32).to_le_bytes()); // color + alpha mask
    data.extend(1u16.to_le_bytes());
    data.extend(32u16.to_le_bytes());
    data.extend(0u32.to_le_bytes());
    data.extend((pixel_bytes + mask_bytes).to_le_bytes());
    for _ in 0..4 { data.extend(0u32.to_le_bytes()); }
    let f = size as f32;
    for y in (0..size).rev() {
        for x in 0..size {
            let xx = (x as f32 + 0.5) / f;
            let yy = (y as f32 + 0.5) / f;
            // Rounded dark-blue tile, with a small white database glyph.
            let corner_dx = (xx - 0.5).abs() - 0.36;
            let corner_dy = (yy - 0.5).abs() - 0.36;
            let d = corner_dx.max(0.0).hypot(corner_dy.max(0.0)) + corner_dx.max(corner_dy).min(0.0);
            let tile = d < 0.105;
            let ellipse = ((xx - 0.5) / 0.285).powi(2) + ((yy - 0.29) / 0.095).powi(2);
            let top = ellipse >= 0.59 && ellipse <= 1.45;
            let side = (xx - 0.5).abs() <= 0.28 && (yy - 0.29) >= 0.0 && yy < 0.70;
            let bottom = ((xx - 0.5) / 0.285).powi(2) + ((yy - 0.69) / 0.095).powi(2);
            let base = bottom >= 0.56 && bottom <= 1.44 && yy > 0.68;
            let stripe = (yy - 0.49).abs() < 0.026 && (xx - 0.5).abs() <= 0.26;
            let edge = side && ((xx - 0.5).abs() > 0.25);
            let glyph = top || base || stripe || edge;
            let color = if glyph { [255, 255, 255, 255] }
                else if tile { [193, 104, 24, 255] } // BGRA = dark blue
                else { [0, 0, 0, 0] };
            data.extend(color);
        }
    }
    data.resize(data.len() + mask_bytes as usize, 0);
    data
}

fn write_ico(path: &PathBuf) -> io::Result<()> {
    let sizes = [16usize, 32, 48, 64];
    let payloads: Vec<Vec<u8>> = sizes.iter().map(|&n| icon_bmp(n)).collect();
    let mut out = Vec::new();
    out.extend(0u16.to_le_bytes());
    out.extend(1u16.to_le_bytes());
    out.extend((sizes.len() as u16).to_le_bytes());
    let mut offset = 6 + sizes.len() * 16;
    for (size, data) in sizes.iter().zip(&payloads) {
        out.push(*size as u8);
        out.push(*size as u8);
        out.push(0);
        out.push(0);
        out.extend(1u16.to_le_bytes());
        out.extend(32u16.to_le_bytes());
        out.extend((data.len() as u32).to_le_bytes());
        out.extend((offset as u32).to_le_bytes());
        offset += data.len();
    }
    for data in payloads { out.extend(data); }
    fs::write(path, out)
}

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let path = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR")).join("mics-data.ico");
        write_ico(&path).expect("Icon creation failed");
        let mut res = winresource::WindowsResource::new();
        res.set_icon(path.to_str().expect("UTF-8 path"));
        res.set("FileDescription", "MICS Data - Firebird Connector");
        res.set("ProductName", "MICS Data");
        res.compile().expect("Windows resource compilation failed");
    }
}
