fn main() {
    println!("cargo:rerun-if-changed=assets/copy-06.svg");
    println!("cargo:rerun-if-changed=build.rs");

    #[cfg(windows)]
    {
        if let Err(e) = embed_windows_icon() {
            panic!("failed to embed Windows icon: {e}");
        }
    }
}

#[cfg(windows)]
fn embed_windows_icon() -> Result<(), Box<dyn std::error::Error>> {
    use std::path::PathBuf;

    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR")?);
    let svg_path = manifest_dir.join("assets").join("copy-06.svg");
    let svg = std::fs::read_to_string(&svg_path)?;
    let svg = svg.replace("currentColor", "#2563EB");

    let out_dir = PathBuf::from(std::env::var("OUT_DIR")?);
    let ico_path = out_dir.join("app.ico");

    let mut icon_dir = ico::IconDir::new(ico::ResourceType::Icon);
    for size in [16u32, 24, 32, 48, 64, 128, 256] {
        let rgba = render_svg(&svg, size)?;
        let image = ico::IconImage::from_rgba_data(size, size, rgba);
        icon_dir.add_entry(ico::IconDirEntry::encode(&image)?);
    }
    let file = std::fs::File::create(&ico_path)?;
    icon_dir.write(file)?;

    let mut res = winresource::WindowsResource::new();
    res.set_icon(ico_path.to_string_lossy().as_ref());
    res.set("ProductName", "MouseCopyPasteDelete");
    res.set("FileDescription", "Mouse copy, paste, and delete selected text by mouse gestures");
    res.compile()?;
    Ok(())
}

#[cfg(windows)]
fn render_svg(svg: &str, size: u32) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    use resvg::tiny_skia::{Pixmap, Transform};
    use resvg::usvg;

    let mut opt = usvg::Options::default();
    opt.resources_dir = None;
    let tree = usvg::Tree::from_str(svg, &opt)?;
    let mut pixmap = Pixmap::new(size, size).ok_or("failed to create pixmap")?;
    let svg_size = tree.size();
    let sx = size as f32 / svg_size.width();
    let sy = size as f32 / svg_size.height();
    let transform = Transform::from_scale(sx, sy);
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    Ok(pixmap.take())
}
