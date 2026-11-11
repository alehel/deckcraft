//! PowerPoint's SVG pictures: an `a:blip` raster fallback with the SVG original in an
//! `asvg:svgBlip` extension.

use std::io::{Cursor, Read, Write};

use deckcraft_geom::Xfrm;
use deckcraft_model::style::PictureFill;
use deckcraft_model::{Presentation, Shape, ShapeId, ShapeKind, defaults};

const SVG: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" width="1" height="1"><rect width="1" height="1" fill="green"/></svg>"#;

fn png_1x1(rgb: [u8; 3]) -> Vec<u8> {
    let mut out = Cursor::new(vec![]);
    image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(1, 1, image::Rgb(rgb))).write_to(&mut out, image::ImageFormat::Png).expect("png");
    out.into_inner()
}

/// The parts of a package, by name.
fn parts(bytes: &[u8]) -> Vec<(String, Vec<u8>)> {
    let mut z = zip::ZipArchive::new(Cursor::new(bytes)).expect("zip");
    (0..z.len())
        .map(|i| {
            let mut f = z.by_index(i).expect("entry");
            let mut d = vec![];
            f.read_to_end(&mut d).expect("read");
            (f.name().to_string(), d)
        })
        .collect()
}

fn part(bytes: &[u8], name: &str) -> String {
    let (_, d) = parts(bytes).into_iter().find(|(n, _)| n == name).expect(name);
    String::from_utf8(d).expect("utf8")
}

fn zip(parts: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut out = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let o = zip::write::SimpleFileOptions::default();
    for (name, d) in parts {
        out.start_file(name.as_str(), o).expect("start");
        out.write_all(d).expect("write");
    }
    out.finish().expect("finish").into_inner()
}

/// A deck with one picture (a 1×1 PNG), as PowerPoint writes an inserted SVG: the PNG in
/// `a:blip`, the SVG part beside it through an `asvg:svgBlip` extension.
fn svg_deck() -> Vec<u8> {
    let mut p = defaults::new_presentation(None);
    let media = p.add_media("image1.png", "image/png", png_1x1([0, 128, 0])).expect("media");
    let id = ShapeId(p.alloc_id().expect("ids"));
    let first = p.slides[0].id;
    p.slide_mut(first).expect("slide").shapes.push(Shape {
        id,
        name: "Logo".into(),
        xfrm: Some(Xfrm::new(10.0, 10.0, 100.0, 100.0)),
        kind: ShapeKind::Picture { fill: PictureFill { media, ..Default::default() } },
        ..Default::default()
    });
    let bytes = deckcraft_pptx::export(&p).expect("export");
    let mut ps = parts(&bytes);
    for (name, d) in &mut ps {
        let s = String::from_utf8(d.clone()).unwrap_or_default();
        let s = match name.as_str() {
            "[Content_Types].xml" => s.replace("<Default ", r#"<Default Extension="svg" ContentType="image/svg+xml"/><Default "#),
            "ppt/slides/_rels/slide1.xml.rels" => s.replace(
                "</Relationships>",
                r#"<Relationship Id="rIdSvg" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="../media/image9.svg"/></Relationships>"#,
            ),
            "ppt/slides/slide1.xml" => {
                let i = s.find("<a:blip ").expect("blip");
                let end = i + s[i..].find("/>").expect("blip end");
                format!(
                    r#"{}><a:extLst><a:ext uri="{{96DAC541-7B7A-43D3-8B79-37D633B846F1}}"><asvg:svgBlip xmlns:asvg="http://schemas.microsoft.com/office/drawing/2016/SVG/main" r:embed="rIdSvg"/></a:ext></a:extLst></a:blip>{}"#,
                    &s[..end],
                    &s[end + 2..]
                )
            }
            _ => continue,
        };
        *d = s.into_bytes();
    }
    ps.push(("ppt/media/image9.svg".into(), SVG.to_vec()));
    zip(&ps)
}

fn picture(p: &Presentation) -> &PictureFill {
    let s = p.slides[0].shapes.iter().find(|s| s.name == "Logo").expect("shape");
    match &s.kind {
        ShapeKind::Picture { fill } => fill,
        other => panic!("{other:?}"),
    }
}

#[test]
fn svg_original_is_read_and_round_trips() {
    let p = deckcraft_pptx::import(&svg_deck()).expect("import");
    let pf = picture(&p);
    let svg = pf.vector().expect("svg original");
    let m = p.media.iter().find(|m| m.id == svg).expect("svg media");
    assert_eq!(m.content_type, "image/svg+xml");
    assert_eq!(m.data.as_slice(), SVG);
    // The raster is still what the picture draws.
    let raster = p.media.iter().find(|m| m.id == pf.media).expect("raster");
    assert_eq!(raster.content_type, "image/png");

    let out = deckcraft_pptx::export(&p).expect("export");
    let slide = part(&out, "ppt/slides/slide1.xml");
    assert!(slide.contains("asvg:svgBlip"), "{slide}");
    assert!(slide.contains("{96DAC541-7B7A-43D3-8B79-37D633B846F1}"), "{slide}");
    let ct = part(&out, "[Content_Types].xml");
    assert!(ct.contains(r#"Extension="svg" ContentType="image/svg+xml""#), "{ct}");
    let rels = part(&out, "ppt/slides/_rels/slide1.xml.rels");
    assert!(rels.contains(".svg\""), "{rels}");

    let q = deckcraft_pptx::import(&out).expect("reimport");
    let qf = picture(&q);
    let svg = qf.vector().expect("svg original after round trip");
    assert_eq!(q.media.iter().find(|m| m.id == svg).expect("svg media").data.as_slice(), SVG);
}

#[test]
fn replaced_picture_drops_its_svg_original() {
    let mut p = deckcraft_pptx::import(&svg_deck()).expect("import");
    let other = p.add_media("other.png", "image/png", png_1x1([200, 0, 0])).expect("media");
    let first = p.slides[0].id;
    let s = p.slide_mut(first).expect("slide");
    let sh = s.shapes.iter_mut().find(|s| s.name == "Logo").expect("shape");
    let ShapeKind::Picture { fill } = &mut sh.kind else { panic!("not a picture") };
    assert!(fill.vector.is_some());
    fill.media = other;
    assert_eq!(fill.vector(), None);

    let out = deckcraft_pptx::export(&p).expect("export");
    let slide = part(&out, "ppt/slides/slide1.xml");
    assert!(!slide.contains("svgBlip"), "{slide}");
    assert!(!parts(&out).iter().any(|(n, _)| n.ends_with(".svg")));
    let q = deckcraft_pptx::import(&out).expect("reimport");
    assert_eq!(picture(&q).vector(), None);
}
