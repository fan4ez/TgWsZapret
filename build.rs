use std::env;
use std::fs;
use std::path::Path;

fn write_bw_e_ico(path: &Path) {
    let width: u32 = 16;
    let height: u32 = 16;
    let xor_size = width * height * 4;
    let and_row_bytes = width.div_ceil(32) * 4;
    let and_size = and_row_bytes * height;
    let dib_header_size: u32 = 40;
    let image_size = dib_header_size + xor_size + and_size;

    let mut ico = Vec::with_capacity(22 + image_size as usize);
    ico.extend_from_slice(&0u16.to_le_bytes());
    ico.extend_from_slice(&1u16.to_le_bytes());
    ico.extend_from_slice(&1u16.to_le_bytes());

    ico.push(width as u8);
    ico.push(height as u8);
    ico.push(0);
    ico.push(0);
    ico.extend_from_slice(&1u16.to_le_bytes());
    ico.extend_from_slice(&32u16.to_le_bytes());
    ico.extend_from_slice(&image_size.to_le_bytes());
    ico.extend_from_slice(&22u32.to_le_bytes());

    ico.extend_from_slice(&dib_header_size.to_le_bytes());
    ico.extend_from_slice(&(width as i32).to_le_bytes());
    ico.extend_from_slice(&((height * 2) as i32).to_le_bytes());
    ico.extend_from_slice(&1u16.to_le_bytes());
    ico.extend_from_slice(&32u16.to_le_bytes());
    ico.extend_from_slice(&0u32.to_le_bytes());
    ico.extend_from_slice(&(xor_size + and_size).to_le_bytes());
    ico.extend_from_slice(&0i32.to_le_bytes());
    ico.extend_from_slice(&0i32.to_le_bytes());
    ico.extend_from_slice(&0u32.to_le_bytes());
    ico.extend_from_slice(&0u32.to_le_bytes());

    for row in 0..height {
        let top_y = height - 1 - row;
        for x in 0..width {
            let is_border = x == 0 || top_y == 0 || x == width - 1 || top_y == height - 1;
            let vert_bar = (4..=5).contains(&x) && (3..=12).contains(&top_y);
            let top_bar = (4..=11).contains(&x) && (3..=4).contains(&top_y);
            let mid_bar = (4..=10).contains(&x) && (7..=8).contains(&top_y);
            let bot_bar = (4..=11).contains(&x) && (11..=12).contains(&top_y);

            let is_black = vert_bar || top_bar || mid_bar || bot_bar;
            let (b, g, r, a) = if is_black {
                (10, 10, 10, 255)
            } else if is_border {
                (210, 210, 210, 255)
            } else {
                (255, 255, 255, 255)
            };
            ico.extend_from_slice(&[b, g, r, a]);
        }
    }

    ico.resize(ico.len() + and_size as usize, 0);
    let _ = fs::write(path, ico);
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let out_dir = env::var("OUT_DIR").expect("OUT_DIR not set");
        let ico_path = Path::new(&out_dir).join("e_bw.ico");
        write_bw_e_ico(&ico_path);

        let manifest = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="requireAdministrator" uiAccess="false"/>
      </requestedPrivileges>
    </security>
  </trustInfo>
</assembly>
"#;

        let mut res = winres::WindowsResource::new();
        if let Some(ico_str) = ico_path.to_str() {
            res.set_icon(ico_str);
        }
        res.set_manifest(manifest);
        if let Err(err) = res.compile() {
            eprintln!("winres warning: {err}");
        }
    }
}
