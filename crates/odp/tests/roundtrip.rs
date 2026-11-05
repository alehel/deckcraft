//! ODP import and export: round trips through our own writer, a hand-written document, and files
//! that must not crash the reader.

use std::io::Write;
use std::sync::Arc;

use deckcraft_engine::Session;
use deckcraft_model::style::{ColorRef, Fill, GradientShape};
use deckcraft_model::table::{Cell, Table};
use deckcraft_model::text::{Bullet, Paragraph, Run, RunProps, TextBody};
use deckcraft_model::{Geom, PhType, Presentation, Rgba, Shape, ShapeId, ShapeKind, Xfrm, defaults};

fn sample() -> Presentation {
    let mut s = Session::new();
    deckcraft_engine::sample::open_sample(&mut s).unwrap();
    (*s.doc().unwrap().doc).clone()
}

fn all_text(p: &Presentation) -> Vec<String> {
    let mut out = vec![];
    for s in &p.slides {
        deckcraft_model::walk(&s.shapes, &mut |sh, _| {
            if let Some(t) = &sh.text {
                let t = t.text();
                if !t.trim().is_empty() {
                    out.push(t);
                }
            }
            if let ShapeKind::Table(t) = &sh.kind {
                for r in &t.rows {
                    for c in &r.cells {
                        out.push(c.text.text());
                    }
                }
            }
        });
    }
    out
}

#[test]
fn the_sample_deck_survives_a_round_trip() {
    let p = sample();
    let bytes = deckcraft_odp::export(&p).unwrap();
    assert!(deckcraft_odp::sniff(&bytes));
    assert!(!deckcraft_pptx::sniff(&bytes));
    // `mimetype` is the first entry, stored, so tools can read the type at a fixed offset.
    assert_eq!(&bytes[30..38], b"mimetype");
    assert_eq!(&bytes[38..38 + deckcraft_odp::MIME_PRESENTATION.len()], deckcraft_odp::MIME_PRESENTATION.as_bytes());
    let back = deckcraft_odp::import(&bytes).unwrap();
    assert_eq!(back.slides.len(), p.slides.len());
    assert!((back.slide_size.width - p.slide_size.width).abs() < 0.01);
    for (a, b) in p.slides.iter().zip(&back.slides) {
        assert_eq!(a.title(), b.title());
        assert_eq!(a.notes.text().trim(), b.notes.text().trim());
    }
    // Every piece of text is still there.
    let (before, after) = (all_text(&p), all_text(&back));
    for t in &before {
        assert!(after.contains(t), "lost text {t:?}");
    }
    // A second trip changes nothing more.
    let again = deckcraft_odp::import(&deckcraft_odp::export(&back).unwrap()).unwrap();
    assert_eq!(all_text(&again), after);
}

fn deck_with(shapes: Vec<Shape>) -> Presentation {
    let mut p = defaults::blank_presentation(defaults::WIDE, Default::default(), true);
    let slide = Arc::make_mut(&mut p.slides[0]);
    slide.shapes.retain(|s| s.ph.is_some());
    for mut s in shapes {
        s.id = ShapeId(p.next_id + 1000 + slide.shapes.len() as u32);
        slide.shapes.push(s);
    }
    p.fix_next_id();
    p
}

fn shapes_of(p: &Presentation) -> Vec<Shape> {
    p.slides[0].shapes.iter().filter(|s| s.ph.is_none()).cloned().collect()
}

#[test]
fn geometry_fill_line_and_rotation_round_trip() {
    let mut star = Shape {
        xfrm: Some(Xfrm { x: 100.0, y: 80.0, w: 200.0, h: 120.0, rot: 30.0, ..Default::default() }),
        geom: Geom::Preset { name: "star5".into(), adj: vec![] },
        fill: Some(Fill::solid(ColorRef::rgb(Rgba::rgb(0x12, 0x34, 0x56)))),
        ..Default::default()
    };
    star.line = Some(deckcraft_model::style::Line::solid(ColorRef::rgb(Rgba::rgb(0xff, 0, 0)), 3.0));
    let mut grad = deckcraft_model::style::Gradient {
        stops: vec![
            deckcraft_model::style::GradientStop { pos: 0.0, color: ColorRef::rgb(Rgba::rgb(255, 0, 0)) },
            deckcraft_model::style::GradientStop { pos: 1.0, color: ColorRef::rgb(Rgba::rgb(0, 0, 255)) },
        ],
        shape: GradientShape::Linear { angle: 45.0, scaled: false },
        flip: None,
        rotate_with_shape: true,
        blend: Default::default(),
        tile: [0.0; 4],
    };
    grad.rotate_with_shape = true;
    let ell = Shape {
        xfrm: Some(Xfrm::new(400.0, 300.0, 100.0, 50.0)),
        geom: Geom::preset("ellipse"),
        fill: Some(Fill::Gradient(grad)),
        ..Default::default()
    };
    let p = deck_with(vec![star, ell]);
    let back = deckcraft_odp::import(&deckcraft_odp::export(&p).unwrap()).unwrap();
    let s = shapes_of(&back);
    assert_eq!(s.len(), 2);
    let x = s[0].xfrm.unwrap();
    assert!((x.x - 100.0).abs() < 0.1 && (x.y - 80.0).abs() < 0.1 && (x.w - 200.0).abs() < 0.1 && (x.h - 120.0).abs() < 0.1, "{x:?}");
    assert!((x.rot - 30.0).abs() < 0.01, "{}", x.rot);
    assert_eq!(s[0].geom.preset_name(), Some("star5"));
    assert_eq!(s[0].fill, Some(Fill::solid(ColorRef::rgb(Rgba::rgb(0x12, 0x34, 0x56)))));
    let l = s[0].line.as_ref().unwrap();
    assert!((l.width.unwrap() - 3.0).abs() < 0.01);
    assert_eq!(s[1].geom.preset_name(), Some("ellipse"));
    let Some(Fill::Gradient(g)) = &s[1].fill else { panic!("{:?}", s[1].fill) };
    let GradientShape::Linear { angle, .. } = g.shape else { panic!() };
    assert!((angle - 45.0).abs() < 0.01, "{angle}");
    assert_eq!(g.stops.first().map(|s| s.color.clone()), Some(ColorRef::rgb(Rgba::rgb(255, 0, 0))));
    assert_eq!(g.stops.last().map(|s| s.color.clone()), Some(ColorRef::rgb(Rgba::rgb(0, 0, 255))));
}

#[test]
fn text_formatting_and_bullets_round_trip() {
    let bold = RunProps {
        bold: Some(true),
        size: Some(32.0),
        font: Some("Lato".into()),
        fill: Some(Fill::solid(ColorRef::rgb(Rgba::rgb(0xc0, 0, 0)))),
        ..Default::default()
    };
    let mut p1 = Paragraph { runs: vec![Run::with("Bold red", bold), Run::new("  and   plain\twith tab")], ..Default::default() };
    p1.props.bullet = Some(Bullet::Char { char: "–".into() });
    p1.props.margin_left = Some(36.0);
    p1.props.indent = Some(-18.0);
    let mut p2 = Paragraph::new("Numbered");
    p2.level = 1;
    p2.props.bullet = Some(Bullet::AutoNum { scheme: "alphaLcParenR".into(), start_at: 3 });
    let tb = Shape {
        xfrm: Some(Xfrm::new(50.0, 50.0, 500.0, 200.0)),
        text_box: true,
        text: Some(TextBody { paragraphs: vec![p1, p2], ..Default::default() }),
        ..Default::default()
    };
    let p = deck_with(vec![tb]);
    let back = deckcraft_odp::import(&deckcraft_odp::export(&p).unwrap()).unwrap();
    let s = shapes_of(&back);
    let t = s[0].text.as_ref().unwrap();
    assert_eq!(t.paragraphs.len(), 2);
    let a = &t.paragraphs[0];
    assert_eq!(a.text(), "Bold red  and   plain\twith tab");
    assert_eq!(a.runs[0].props.bold, Some(true));
    assert_eq!(a.runs[0].props.size, Some(32.0));
    assert_eq!(a.runs[0].props.font.as_deref(), Some("Lato"));
    assert_eq!(a.runs[0].props.fill, Some(Fill::solid(ColorRef::rgb(Rgba::rgb(0xc0, 0, 0)))));
    assert_eq!(a.props.bullet, Some(Bullet::Char { char: "–".into() }));
    assert!((a.props.margin_left.unwrap() - 36.0).abs() < 0.01);
    assert!((a.props.indent.unwrap() + 18.0).abs() < 0.01);
    let b = &t.paragraphs[1];
    assert_eq!(b.level, 1);
    assert_eq!(b.props.bullet, Some(Bullet::AutoNum { scheme: "alphaLcParenR".into(), start_at: 3 }));
}

#[test]
fn tables_and_pictures_round_trip() {
    let mut t = Table::new(2, 3, 300.0, 40.0);
    t.rows[0].cells[0] = Cell::new("Region");
    t.rows[1].cells[2] = Cell::new("19");
    t.rows[0].cells[1].fill = Some(Fill::solid(ColorRef::rgb(Rgba::rgb(0x2a, 0x60, 0x99))));
    t.merge(1, 0, 1, 1);
    t.first_row = true;
    t.band_row = true;
    let table = Shape { xfrm: Some(Xfrm::new(20.0, 20.0, 300.0, 80.0)), kind: ShapeKind::Table(t), ..Default::default() };
    let mut p = deck_with(vec![table]);
    // A 1×1 PNG.
    let png: Vec<u8> = vec![
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0x0d, 0x49, 0x48, 0x44, 0x52, 0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0, 0x1f, 0x15,
        0xc4, 0x89, 0, 0, 0, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0xf0, 0x1f, 0, 0x05, 0, 0x01, 0xff, 0x89, 0x99, 0x3d,
        0x1d, 0, 0, 0, 0, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];
    let media = p.add_media("dot.png", "image/png", png.clone()).unwrap();
    let pic = Shape {
        id: ShapeId(p.alloc_id().unwrap()),
        xfrm: Some(Xfrm::new(400.0, 100.0, 64.0, 64.0)),
        kind: ShapeKind::Picture { fill: deckcraft_model::style::PictureFill { media, ..Default::default() } },
        descr: "A dot".into(),
        ..Default::default()
    };
    Arc::make_mut(&mut p.slides[0]).shapes.push(pic);
    let back = deckcraft_odp::import(&deckcraft_odp::export(&p).unwrap()).unwrap();
    let s = shapes_of(&back);
    let ShapeKind::Table(t) = &s[0].kind else { panic!("{:?}", s[0].kind) };
    assert_eq!((t.n_rows(), t.n_cols(), t.first_row, t.band_row), (2, 3, true, true));
    assert_eq!(t.rows[0].cells[0].text.text(), "Region");
    assert_eq!(t.rows[1].cells[2].text.text(), "19");
    assert_eq!(t.rows[1].cells[0].grid_span, 2);
    assert!(t.rows[1].cells[1].h_merge);
    assert_eq!(t.rows[0].cells[1].fill, Some(Fill::solid(ColorRef::rgb(Rgba::rgb(0x2a, 0x60, 0x99)))));
    assert!((t.cols[0] - 100.0).abs() < 0.1);
    let ShapeKind::Picture { fill } = &s[1].kind else { panic!("{:?}", s[1].kind) };
    assert_eq!(back.media(fill.media).map(|m| m.data.to_vec()), Some(png));
    assert_eq!(s[1].descr, "A dot");
}

#[test]
fn placeholders_keep_their_kind_and_the_master_its_background() {
    let mut p = defaults::new_presentation(None);
    let m = Arc::make_mut(&mut p.masters[0]);
    m.background = Some(deckcraft_model::Background::Fill { fill: Fill::solid(ColorRef::rgb(Rgba::rgb(0x20, 0x30, 0x40))) });
    let s = Arc::make_mut(&mut p.slides[0]);
    for sh in &mut s.shapes {
        if sh.ph_type().is_some_and(PhType::is_title) {
            sh.text = Some(TextBody::from_text("Hello"));
        }
    }
    let back = deckcraft_odp::import(&deckcraft_odp::export(&p).unwrap()).unwrap();
    assert_eq!(back.slides[0].title(), "Hello");
    let bg = back.masters[0].background.clone();
    assert_eq!(bg, Some(deckcraft_model::Background::Fill { fill: Fill::solid(ColorRef::rgb(Rgba::rgb(0x20, 0x30, 0x40))) }));
}

const FLAT: &str = r##"<?xml version="1.0" encoding="UTF-8"?>
<o:document xmlns:o="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:s="urn:oasis:names:tc:opendocument:xmlns:style:1.0" xmlns:t="urn:oasis:names:tc:opendocument:xmlns:text:1.0" xmlns:d="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0" xmlns:f="urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0" xmlns:v="urn:oasis:names:tc:opendocument:xmlns:svg-compatible:1.0" xmlns:p="urn:oasis:names:tc:opendocument:xmlns:presentation:1.0" o:mimetype="application/vnd.oasis.opendocument.presentation">
 <o:styles>
  <s:default-style s:family="graphic"><s:text-properties f:font-size="20pt"/></s:default-style>
  <s:style s:name="base" s:family="graphic"><s:graphic-properties d:fill="solid" d:fill-color="#00ff00" d:stroke="none"/><s:text-properties f:color="#333333"/></s:style>
 </o:styles>
 <o:automatic-styles>
  <s:page-layout s:name="PM1"><s:page-layout-properties f:page-width="25.4cm" f:page-height="19.05cm"/></s:page-layout>
  <s:style s:name="gr1" s:family="graphic" s:parent-style-name="base"><s:graphic-properties d:fill-color="#ff0000"/></s:style>
  <s:style s:name="T1" s:family="text"><s:text-properties f:font-weight="bold" f:font-size="50%"/></s:style>
  <t:list-style s:name="L1"><t:list-level-style-bullet t:level="1" t:bullet-char="✓"><s:list-level-properties t:space-before="0.5cm" t:min-label-width="0.5cm"/></t:list-level-style-bullet></t:list-style>
 </o:automatic-styles>
 <o:master-styles><s:master-page s:name="Default" s:page-layout-name="PM1"/></o:master-styles>
 <o:body><o:presentation>
  <d:page d:name="intro" d:master-page-name="Default">
   <d:rect d:style-name="gr1" v:x="1in" v:y="1in" v:width="2in" v:height="1in"><t:p>  Hello   <t:span t:style-name="T1">big</t:span><t:s t:c="2"/>world  </t:p></d:rect>
   <d:frame d:style-name="gr1" v:width="2in" v:height="1in" d:transform="rotate (1.5707963) translate (3in 4in)"><d:text-box><t:list t:style-name="L1"><t:list-item><t:p>Checked</t:p></t:list-item></t:list></d:text-box></d:frame>
  </d:page>
 </o:presentation></o:body>
</o:document>"##;

#[test]
fn a_flat_document_with_other_prefixes_reads() {
    assert!(deckcraft_odp::sniff(FLAT.as_bytes()));
    let p = deckcraft_odp::import(FLAT.as_bytes()).unwrap();
    assert!((p.slide_size.width - 720.0).abs() < 0.01 && (p.slide_size.height - 540.0).abs() < 0.01);
    assert_eq!(p.slides.len(), 1);
    let sh = &p.slides[0].shapes;
    assert_eq!(sh.len(), 2);
    // Style chain: the automatic style's red fill over its parent's green.
    assert_eq!(sh[0].fill, Some(Fill::solid(ColorRef::rgb(Rgba::rgb(255, 0, 0)))));
    let x = sh[0].xfrm.unwrap();
    assert!((x.x - 72.0).abs() < 0.01 && (x.w - 144.0).abs() < 0.01);
    let para = &sh[0].text.as_ref().unwrap().paragraphs[0];
    // White space collapses; text:s keeps its spaces; a relative size halves the default.
    assert_eq!(para.text(), "Hello big  world");
    let big = para.runs.iter().find(|r| r.text == "big").unwrap();
    assert_eq!(big.props.bold, Some(true));
    assert_eq!(big.props.size, Some(10.0));
    assert_eq!(para.runs[0].props.fill, Some(Fill::solid(ColorRef::rgb(Rgba::rgb(0x33, 0x33, 0x33)))));
    // A quarter turn counter-clockwise.
    let x = sh[1].xfrm.unwrap();
    assert!((x.rot - 270.0).abs() < 0.01, "{}", x.rot);
    let item = &sh[1].text.as_ref().unwrap().paragraphs[0];
    assert_eq!(item.props.bullet, Some(Bullet::Char { char: "✓".into() }));
}

#[test]
fn hostile_files_are_errors_not_crashes() {
    assert!(deckcraft_odp::import(b"").is_err());
    assert!(deckcraft_odp::import(b"PK\x03\x04garbage").is_err());
    // A zip with a presentation mimetype but no content.
    let mut z = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    z.start_file("mimetype", zip::write::SimpleFileOptions::default()).unwrap();
    z.write_all(deckcraft_odp::MIME_PRESENTATION.as_bytes()).unwrap();
    let bytes = z.finish().unwrap().into_inner();
    assert!(deckcraft_odp::sniff(&bytes));
    assert!(deckcraft_odp::import(&bytes).is_err());
    // Deep nesting, absurd numbers and self-referencing equations.
    let deep = format!(
        r#"<office:document xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0" xmlns:svg="urn:oasis:names:tc:opendocument:xmlns:svg-compatible:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0"><office:body><office:presentation><draw:page>{}<draw:custom-shape svg:width="1e308cm" svg:height="NaNcm" svg:x="-1e400in"><draw:enhanced-geometry draw:enhanced-path="M ?a ?a L $9 ?b A 0 0 0 0 0 0 0 0 W 1 1 1 1 1 1 1 1 T 1 1 1 1 0 99999 Z" draw:modifiers="nan inf"><draw:equation draw:name="a" draw:formula="?b*?a/0"/><draw:equation draw:name="b" draw:formula="if(?a,sqrt(-1),atan2(0,0)))))(((("/></draw:enhanced-geometry><text:p>{}</text:p></draw:custom-shape>{}</draw:page></office:presentation></office:body></office:document>"#,
        "<draw:g>".repeat(200),
        "<text:span>".repeat(200),
        "</draw:g>".repeat(200)
    );
    if let Ok(p) = deckcraft_odp::import(deep.as_bytes()) {
        let _ = deckcraft_odp::export(&p).unwrap();
    }
}

const NS: &str = r#"xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0" xmlns:svg="urn:oasis:names:tc:opendocument:xmlns:svg-compatible:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0" xmlns:chart="urn:oasis:names:tc:opendocument:xmlns:chart:1.0" xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0" xmlns:fo="urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0""#;

/// Repeat counts and spans in the billions are bounded, not expanded: a table and a chart that
/// claim them read quickly into something small.
#[test]
fn huge_repeats_and_spans_are_bounded() {
    let doc = format!(
        r#"<office:document {NS} office:mimetype="application/vnd.oasis.opendocument.presentation"><office:body><office:presentation><draw:page>
<draw:frame svg:x="1cm" svg:y="1cm" svg:width="10cm" svg:height="5cm"><table:table>
<table:table-column table:number-columns-repeated="2000000000"/>
<table:table-row table:number-rows-repeated="2000000000"><table:table-cell table:number-columns-repeated="2000000000" table:number-columns-spanned="4000000000" table:number-rows-spanned="4000000000"><text:p>x</text:p></table:table-cell></table:table-row>
</table:table></draw:frame>
<draw:frame svg:x="1cm" svg:y="7cm" svg:width="10cm" svg:height="5cm"><draw:object><office:document><office:body><office:chart><chart:chart chart:class="chart:bar"><chart:plot-area><chart:series chart:values-cell-range-address="local-table.$B$1:.$B$2000000000"><chart:data-point chart:repeated="2000000000"/></chart:series></chart:plot-area>
<table:table table:name="local-table"><table:table-rows><table:table-row table:number-rows-repeated="2000000000"><table:table-cell table:number-columns-repeated="2000000000" office:value-type="float" office:value="1"><text:p>1</text:p></table:table-cell></table:table-row></table:table-rows></table:table>
</chart:chart></office:chart></office:body></office:document></draw:object></draw:frame>
</draw:page></office:presentation></office:body></office:document>"#
    );
    let started = std::time::Instant::now();
    let p = deckcraft_odp::import(doc.as_bytes()).unwrap();
    assert!(started.elapsed().as_secs() < 10);
    let mut cells = 0;
    let mut values = 0;
    deckcraft_model::walk(&p.slides[0].shapes, &mut |sh, _| match &sh.kind {
        ShapeKind::Table(t) => {
            for r in &t.rows {
                assert!(r.cells.len() == t.cols.len());
                for (ci, c) in r.cells.iter().enumerate() {
                    assert!(ci + c.grid_span as usize <= t.cols.len());
                }
                cells += r.cells.len();
            }
        }
        ShapeKind::Chart(c) => values += c.series.iter().map(|s| s.values.len()).sum::<usize>(),
        _ => {}
    });
    assert!(cells > 0 && cells <= 1_000_000, "{cells}");
    assert!(values <= 4096, "{values}");
    deckcraft_odp::export(&p).unwrap();
}

/// Repeats that cost nothing to write must still cost something to read: empty chart rows,
/// cells padded out to a wide table, and big text copied into every repeated cell.
#[test]
fn cheap_repeats_are_charged_their_real_cost() {
    let page = |body: String| {
        format!(
            r#"<office:document {NS} office:mimetype="application/vnd.oasis.opendocument.presentation"><office:body><office:presentation><draw:page>{body}</draw:page></office:presentation></office:body></office:document>"#
        )
    };
    let empty_rows = r#"<table:table-row table:number-rows-repeated="65536"/>"#.repeat(4000);
    let chart = format!(
        r#"<draw:frame svg:width="10cm" svg:height="5cm"><draw:object><office:document><office:body><office:chart><chart:chart chart:class="chart:bar"><chart:plot-area/><table:table table:name="local-table"><table:table-rows>{empty_rows}</table:table-rows></table:table></chart:chart></office:chart></office:body></office:document></draw:object></draw:frame>"#
    );
    let wide = r#"<draw:frame svg:width="10cm" svg:height="5cm"><table:table><table:table-column table:number-columns-repeated="1000"/><table:table-row table:number-rows-repeated="1000"><table:table-cell table:number-columns-repeated="1000"/></table:table-row></table:table></draw:frame>"#;
    let text = format!(
        r#"<draw:frame svg:width="10cm" svg:height="5cm"><table:table><table:table-column table:number-columns-repeated="1000"/><table:table-row table:number-rows-repeated="1000"><table:table-cell table:number-columns-repeated="1000"><text:p>{}</text:p></table:table-cell></table:table-row></table:table></draw:frame>"#,
        "x".repeat(20_000)
    );
    for doc in [page(chart), page(wide.to_string()), page(text)] {
        let started = std::time::Instant::now();
        let p = deckcraft_odp::import(doc.as_bytes()).unwrap();
        let mut cells = 0usize;
        let mut chars = 0usize;
        deckcraft_model::walk(&p.slides[0].shapes, &mut |sh, _| {
            if let ShapeKind::Table(t) = &sh.kind {
                for r in &t.rows {
                    cells += r.cells.len();
                    chars += r.cells.iter().map(|c| c.text.text().len()).sum::<usize>();
                }
            }
        });
        assert!(cells <= 65_536 && chars <= 5 << 20, "{cells} cells, {chars} chars");
        assert!(started.elapsed().as_secs() < 10, "{:?}", started.elapsed());
    }
}

/// `text:s` elements each ask for up to a thousand spaces; a document gets a few million in all.
#[test]
fn expanded_spaces_have_a_document_budget() {
    let frame = format!(
        r#"<draw:frame svg:width="10cm" svg:height="5cm"><draw:text-box><text:p>{}</text:p></draw:text-box></draw:frame>"#,
        r#"<text:s text:c="1000"/>"#.repeat(10_000)
    );
    let doc = format!(
        r#"<office:document {NS} office:mimetype="application/vnd.oasis.opendocument.presentation"><office:body><office:presentation><draw:page>{}</draw:page></office:presentation></office:body></office:document>"#,
        frame.repeat(10)
    );
    let p = deckcraft_odp::import(doc.as_bytes()).unwrap();
    let total: usize = p.slides[0].shapes.iter().filter_map(|s| s.text.as_ref()).map(|t| t.text().len()).sum();
    assert!(total <= (1 << 22) + 200_000, "{total}");
}

/// A cell whose style says `transparent` has no fill; a trailing space in a paragraph stays.
#[test]
fn transparent_cells_and_trailing_spaces_read() {
    let doc = format!(
        r#"<office:document {NS} office:mimetype="application/vnd.oasis.opendocument.presentation"><office:automatic-styles><style:style style:name="ce1" style:family="table-cell"><style:table-cell-properties fo:background-color="transparent"/></style:style></office:automatic-styles><office:body><office:presentation><draw:page>
<draw:frame svg:x="1cm" svg:y="1cm" svg:width="10cm" svg:height="5cm"><table:table><table:table-column/><table:table-row><table:table-cell table:style-name="ce1"><text:p>x</text:p></table:table-cell></table:table-row></table:table></draw:frame>
<draw:frame svg:x="1cm" svg:y="7cm" svg:width="10cm" svg:height="2cm"><draw:text-box><text:p>ends with<text:s/></text:p></draw:text-box></draw:frame>
</draw:page></office:presentation></office:body></office:document>"#
    );
    let p = deckcraft_odp::import(doc.as_bytes()).unwrap();
    let sh = &p.slides[0].shapes;
    let ShapeKind::Table(t) = &sh[0].kind else { panic!("not a table") };
    assert!(matches!(t.rows[0].cells[0].fill, None | Some(Fill::None)), "{:?}", t.rows[0].cells[0].fill);
    assert_eq!(sh[1].text.as_ref().unwrap().paragraphs[0].text(), "ends with ");
}

/// An outline paragraph out of a list has no indent, though its master level's list has one;
/// level 2 of a list takes the second outline style, not the first.
#[test]
fn outline_levels_resolve_from_the_master() {
    let doc = format!(
        r#"<office:document {NS} xmlns:presentation="urn:oasis:names:tc:opendocument:xmlns:presentation:1.0" office:mimetype="application/vnd.oasis.opendocument.presentation"><office:styles>
<style:style style:name="M-outline1" style:family="presentation"><style:graphic-properties><text:list-style style:name="L"><text:list-level-style-bullet text:level="1" text:bullet-char="●"><style:list-level-properties text:space-before="1cm" text:min-label-width="0.5cm"/></text:list-level-style-bullet><text:list-level-style-bullet text:level="2" text:bullet-char="–"><style:list-level-properties text:space-before="2cm" text:min-label-width="0.5cm"/></text:list-level-style-bullet></text:list-style></style:graphic-properties><style:text-properties fo:font-size="32pt"/></style:style>
<style:style style:name="M-outline2" style:family="presentation" style:parent-style-name="M-outline1"><style:text-properties fo:font-size="20pt"/></style:style>
</office:styles><office:master-styles><style:master-page style:name="M"/></office:master-styles><office:body><office:presentation><draw:page draw:master-page-name="M">
<draw:frame presentation:class="outline" presentation:style-name="M-outline1" svg:x="1cm" svg:y="1cm" svg:width="20cm" svg:height="10cm"><draw:text-box><text:p>plain</text:p><text:list><text:list-item><text:p>one</text:p><text:list><text:list-item><text:p>two</text:p></text:list-item></text:list></text:list-item></text:list></draw:text-box></draw:frame>
</draw:page></office:presentation></office:body></office:document>"#
    );
    let p = deckcraft_odp::import(doc.as_bytes()).unwrap();
    let paras = &p.slides[0].shapes.iter().find_map(|s| s.text.clone()).unwrap().paragraphs;
    assert_eq!(paras[0].props.margin_left, Some(0.0));
    assert_eq!(paras[0].props.bullet, Some(Bullet::None));
    let size = |i: usize| paras[i].runs[0].props.size;
    assert_eq!((size(1), size(2)), (Some(32.0), Some(20.0)));
    assert!(paras[2].props.margin_left.unwrap() > paras[1].props.margin_left.unwrap());
}

/// Auto-numbers continue across paragraphs and restart only where a new run begins: after a
/// paragraph without a number, or at a different start.
#[test]
fn numbering_runs_restart_only_where_they_begin() {
    let para = |t: &str, start: Option<u32>| {
        let mut p = Paragraph { runs: vec![Run::new(t)], ..Default::default() };
        p.props.bullet = Some(match start {
            Some(s) => Bullet::AutoNum { scheme: "arabicPeriod".into(), start_at: s },
            None => Bullet::None,
        });
        p
    };
    // Where the numbering restarts (a list that doesn't continue the one before), and the deck
    // read back.
    let restarts = |paras: Vec<Paragraph>| {
        let shape = Shape {
            xfrm: Some(Xfrm::new(50.0, 50.0, 300.0, 200.0)),
            text_box: true,
            text: Some(TextBody { paragraphs: paras, ..Default::default() }),
            ..Default::default()
        };
        let bytes = deckcraft_odp::export(&deck_with(vec![shape])).unwrap();
        let xml = part(&bytes, "content.xml");
        let page = &xml[xml.find("<draw:page ").unwrap()..];
        let n = page.matches(r#"text:continue-numbering="false""#).count();
        let back = deckcraft_odp::import(&bytes).unwrap();
        let starts: Vec<Option<u32>> = shapes_of(&back)[0]
            .text
            .as_ref()
            .unwrap()
            .paragraphs
            .iter()
            .map(|p| match &p.props.bullet {
                Some(Bullet::AutoNum { start_at, .. }) => Some(*start_at),
                _ => None,
            })
            .collect();
        (n, starts)
    };
    let three = restarts(vec![para("a", Some(3)), para("b", Some(3)), para("c", Some(3))]);
    assert_eq!(three, (1, vec![Some(3); 3]));
    let mixed = vec![para("a", Some(1)), para("b", Some(1)), para("plain", None), para("c", Some(1)), para("d", Some(7)), para("e", Some(7))];
    assert_eq!(restarts(mixed), (3, vec![Some(1), Some(1), None, Some(1), Some(7), Some(7)]));
}

/// Page names stay unique even when a slide's own name looks like another's numbered one.
#[test]
fn page_names_are_unique() {
    let mut p = defaults::new_presentation(None);
    let first = p.slides[0].clone();
    p.slides = ["A (3)", "A", "A"]
        .iter()
        .enumerate()
        .map(|(i, n)| {
            let mut s = (*first).clone();
            s.id = deckcraft_model::SlideId(9000 + i as u32);
            s.name = n.to_string();
            Arc::new(s)
        })
        .collect();
    p.fix_next_id();
    let xml = part(&deckcraft_odp::export(&p).unwrap(), "content.xml");
    let names: Vec<String> = tags(&xml)
        .into_iter()
        .filter(|(t, _)| t == "draw:page")
        .filter_map(|(_, a)| a.into_iter().find(|(k, _)| k == "draw:name").map(|(_, v)| v))
        .collect();
    let unique: std::collections::HashSet<&String> = names.iter().collect();
    assert_eq!((names.len(), unique.len()), (3, 3), "{names:?}");
}

/// An effect on a paragraph deep in nested lists finds that paragraph: ids come from the same
/// walk that reads the paragraphs.
#[test]
fn paragraph_targets_match_deeply_nested_paragraphs() {
    let depth = 40;
    let open = "<text:list><text:list-item>".repeat(depth);
    let close = "</text:list-item></text:list>".repeat(depth);
    let doc = format!(
        r#"<office:document {NS} xmlns:presentation="urn:oasis:names:tc:opendocument:xmlns:presentation:1.0" xmlns:anim="urn:oasis:names:tc:opendocument:xmlns:animation:1.0" xmlns:smil="urn:oasis:names:tc:opendocument:xmlns:smil-compatible:1.0" office:mimetype="application/vnd.oasis.opendocument.presentation"><office:body><office:presentation><draw:page>
<draw:frame xml:id="f1" svg:width="10cm" svg:height="5cm"><draw:text-box><text:p>top</text:p>{open}<text:p xml:id="deep">deep</text:p>{close}</draw:text-box></draw:frame>
<anim:par presentation:node-type="timing-root"><anim:seq presentation:node-type="main-sequence"><anim:par smil:begin="indefinite"><anim:par smil:begin="0s"><anim:par smil:begin="0s" presentation:node-type="on-click" presentation:preset-class="entrance" presentation:preset-id="ooo-entrance-appear"><anim:set smil:dur="0.001s" smil:targetElement="deep" smil:attributeName="visibility" smil:to="visible"/></anim:par></anim:par></anim:par></anim:seq></anim:par>
</draw:page></office:presentation></office:body></office:document>"#
    );
    let p = deckcraft_odp::import(doc.as_bytes()).unwrap();
    let s = &p.slides[0];
    let a = s.animations.first().expect("the effect reads");
    let text = s.shapes[0].text.as_ref().unwrap();
    let i = a.paragraph.unwrap() as usize;
    assert_eq!(text.paragraphs[i].text(), "deep");
}

/// A presentation without masters still writes a package LibreOffice opens.
#[test]
fn a_deck_without_masters_exports() {
    let mut p = defaults::new_presentation(None);
    p.masters.clear();
    let back = deckcraft_odp::import(&deckcraft_odp::export(&p).unwrap()).unwrap();
    assert_eq!(back.slides.len(), p.slides.len());
}

/// The start tags of an XML document: name and attributes.
fn tags(xml: &str) -> Vec<(String, Vec<(String, String)>)> {
    let mut out = vec![];
    for chunk in xml.split('<').skip(1) {
        if chunk.starts_with(['/', '?', '!']) {
            continue;
        }
        let end = chunk.find('>').unwrap();
        let body = chunk[..end].trim_end_matches('/');
        let name_end = body.find(char::is_whitespace).unwrap_or(body.len());
        let mut attrs = vec![];
        let mut rest = &body[name_end..];
        while let Some(eq) = rest.find('=') {
            let name = rest[..eq].trim().to_string();
            let q = rest[eq + 1..].chars().next().unwrap();
            let after = &rest[eq + 2..];
            let close = after.find(q).unwrap();
            attrs.push((name, after[..close].to_string()));
            rest = &after[close + 1..];
        }
        out.push((body[..name_end].to_string(), attrs));
    }
    out
}

fn part(bytes: &[u8], name: &str) -> String {
    let mut z = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
    let mut s = String::new();
    std::io::Read::read_to_string(&mut z.by_name(name).unwrap(), &mut s).unwrap();
    s
}

/// What we write has no repeated attribute, unique ids, and no reference to an id it didn't write
/// — even when an effect targets a footer placeholder, which a slide doesn't write.
#[test]
fn written_ids_are_unique_and_references_resolve() {
    use deckcraft_model::anim::Animation;
    let mut p = sample();
    let slide = Arc::make_mut(&mut p.slides[0]);
    let footer = Shape { ph: Some(deckcraft_model::Placeholder { kind: PhType::Footer, ..Default::default() }), ..Default::default() };
    let footer_id = ShapeId(90_000);
    slide.shapes.push(Shape { id: footer_id, ..footer });
    let first = slide.shapes[0].id;
    slide.animations.push(Animation { shape: footer_id, ..Default::default() });
    slide.animations.push(Animation { shape: first, trigger: Some(footer_id), ..Default::default() });
    slide.animations.push(Animation { shape: first, paragraph: Some(999), ..Default::default() });
    p.fix_next_id();
    let bytes = deckcraft_odp::export(&p).unwrap();
    for name in ["content.xml", "styles.xml"] {
        let xml = part(&bytes, name);
        let tags = tags(&xml);
        let mut ids = std::collections::HashSet::new();
        for (tag, attrs) in &tags {
            let mut names = std::collections::HashSet::new();
            for (k, v) in attrs {
                assert!(names.insert(k.as_str()), "{name}: <{tag}> repeats {k}");
                if k == "xml:id" {
                    assert!(ids.insert(v.clone()), "{name}: id {v} written twice");
                }
            }
        }
        for (tag, attrs) in &tags {
            for (k, v) in attrs {
                let id = match k.as_str() {
                    "smil:targetElement" | "draw:start-shape" | "draw:end-shape" => v.as_str(),
                    "smil:begin" if v.ends_with(".click") || v.ends_with(".begin") => v.rsplit_once('.').unwrap().0,
                    _ => continue,
                };
                assert!(ids.contains(id), "{name}: <{tag} {k}={v}> points at nothing");
            }
        }
    }
    deckcraft_odp::import(&bytes).unwrap();
}

#[test]
fn templates_write_the_template_media_type() {
    let p = defaults::new_presentation(None);
    let bytes = deckcraft_odp::export_as(&p, "otp").unwrap();
    assert_eq!(&bytes[38..38 + deckcraft_odp::MIME_TEMPLATE.len()], deckcraft_odp::MIME_TEMPLATE.as_bytes());
    assert!(deckcraft_odp::sniff(&bytes));
    assert_eq!(deckcraft_odp::import(&bytes).unwrap().slides.len(), 1);
}

#[test]
fn charts_round_trip_as_chart_documents() {
    use deckcraft_model::chart::{Chart, ChartType, Series};
    for kind in [ChartType::Column, ChartType::StackedBar, ChartType::LineMarkers, ChartType::Pie, ChartType::Area] {
        let chart = Chart {
            kind,
            title: Some("Sales".into()),
            categories: vec!["Q1".into(), "Q2".into(), "Q3".into()],
            series: vec![
                Series { name: "North".into(), values: vec![Some(1.0), Some(2.5), None], ..Default::default() },
                Series { name: "South".into(), values: vec![Some(3.0), Some(0.0), Some(-1.0)], ..Default::default() },
            ],
            legend: Some("b".into()),
            ..Default::default()
        };
        let shape = Shape {
            xfrm: Some(Xfrm::new(50.0, 50.0, 400.0, 300.0)),
            kind: ShapeKind::Chart(Box::new(chart)),
            descr: "Sales by quarter".into(),
            ..Default::default()
        };
        let p = deck_with(vec![shape]);
        let bytes = deckcraft_odp::export(&p).unwrap();
        let back = deckcraft_odp::import(&bytes).unwrap();
        let s = shapes_of(&back);
        let ShapeKind::Chart(c) = &s[0].kind else { panic!("{kind:?}: {:?}", s[0].kind) };
        let want = if kind == ChartType::Pie { 1 } else { 2 };
        assert!(c.series.len() >= want, "{kind:?}");
        assert_eq!(c.kind, kind, "{kind:?}");
        assert_eq!(c.title.as_deref(), Some("Sales"));
        assert_eq!(c.categories, vec!["Q1", "Q2", "Q3"]);
        assert_eq!(c.series[0].name, "North");
        assert_eq!(c.series[0].values, vec![Some(1.0), Some(2.5), None]);
        assert_eq!(c.legend.as_deref(), Some("b"));
        // Its frame keeps the alt text, like any other frame.
        assert_eq!(s[0].descr, "Sales by quarter");
    }
}

#[test]
fn animations_and_transitions_round_trip() {
    use deckcraft_model::anim::{AnimClass, AnimStart, Animation};
    let a = Shape { xfrm: Some(Xfrm::new(50.0, 50.0, 100.0, 80.0)), geom: Geom::preset("rect"), ..Default::default() };
    let b = Shape {
        xfrm: Some(Xfrm::new(200.0, 50.0, 300.0, 200.0)),
        text_box: true,
        text: Some(TextBody::from_text("one\ntwo\nthree")),
        ..Default::default()
    };
    let mut p = deck_with(vec![a, b]);
    let ids: Vec<ShapeId> = shapes_of(&p).iter().map(|s| s.id).collect();
    let anims = vec![
        Animation { shape: ids[0], effect: "fly".into(), option: "l".into(), duration_ms: 750, ..Default::default() },
        Animation { shape: ids[1], effect: "fade".into(), paragraph: Some(1), start: AnimStart::WithPrevious, delay_ms: 250, ..Default::default() },
        Animation {
            shape: ids[0],
            class: AnimClass::Emphasis,
            effect: "spin".into(),
            option: "counterClockwise".into(),
            start: AnimStart::AfterPrevious,
            duration_ms: 2000,
            ..Default::default()
        },
        Animation { shape: ids[1], class: AnimClass::Exit, effect: "wipeOut".into(), option: "t".into(), ..Default::default() },
        Animation {
            shape: ids[0],
            class: AnimClass::Path,
            effect: "customPath".into(),
            path: Some("M 0 0 L 0.25 0.1 E".into()),
            duration_ms: 2000,
            ..Default::default()
        },
        Animation { shape: ids[1], effect: "zoom".into(), trigger: Some(ids[0]), ..Default::default() },
    ];
    let slide = Arc::make_mut(&mut p.slides[0]);
    slide.animations = anims.clone();
    slide.transition = Some(deckcraft_model::Transition { kind: "push".into(), option: "u".into(), duration_ms: 1200, ..Default::default() });
    let back = deckcraft_odp::import(&deckcraft_odp::export(&p).unwrap()).unwrap();
    let s = &back.slides[0];
    let t = s.transition.as_ref().unwrap();
    assert_eq!((t.kind.as_str(), t.option.as_str(), t.duration_ms), ("push", "u", 1200));
    let new_ids: Vec<ShapeId> = shapes_of(&back).iter().map(|s| s.id).collect();
    let map = |id: ShapeId| new_ids[ids.iter().position(|i| *i == id).unwrap()];
    assert_eq!(s.animations.len(), anims.len());
    for (want, got) in anims.iter().zip(&s.animations) {
        assert_eq!(got.shape, map(want.shape), "{}", want.effect);
        assert_eq!((got.class, got.effect.as_str(), got.option.as_str()), (want.class, want.effect.as_str(), want.option.as_str()));
        assert_eq!(got.start, want.start, "{}", want.effect);
        assert_eq!(got.duration_ms, want.duration_ms, "{}", want.effect);
        assert_eq!(got.delay_ms, want.delay_ms, "{}", want.effect);
        assert_eq!(got.paragraph, want.paragraph, "{}", want.effect);
        assert_eq!(got.trigger, want.trigger.map(map), "{}", want.effect);
    }
    assert_eq!(s.animations[4].path.as_deref(), Some("M 0 0 L 0.25 0.1 E"));
}

/// Repeats, reversal, easing, rewind, amounts and colours live on the effect node and survive.
#[test]
fn animation_modifiers_and_amounts_round_trip() {
    use deckcraft_model::anim::{AnimClass, Animation};
    use deckcraft_model::style::ColorRef;
    let a = Shape { xfrm: Some(Xfrm::new(50.0, 50.0, 100.0, 80.0)), geom: Geom::preset("rect"), ..Default::default() };
    let mut p = deck_with(vec![a]);
    let id = shapes_of(&p)[0].id;
    let anims = vec![
        Animation {
            shape: id,
            effect: "wipe".into(),
            option: "r".into(),
            repeat: 3,
            auto_reverse: true,
            smooth_start: 0.25,
            smooth_end: 0.5,
            ..Default::default()
        },
        Animation {
            shape: id,
            class: AnimClass::Emphasis,
            effect: "spin".into(),
            option: "counterClockwise".into(),
            amount: Some(90.0),
            repeat: u32::MAX,
            rewind: true,
            duration_ms: 2000,
            ..Default::default()
        },
        Animation {
            shape: id,
            class: AnimClass::Emphasis,
            effect: "growShrink".into(),
            option: "horz".into(),
            amount: Some(2.0),
            ..Default::default()
        },
        Animation { shape: id, class: AnimClass::Emphasis, effect: "transparency".into(), amount: Some(0.75), ..Default::default() },
        Animation {
            shape: id,
            class: AnimClass::Emphasis,
            effect: "fillColor".into(),
            color: Some(ColorRef::rgb(deckcraft_color::Rgba::rgb(0x12, 0x34, 0x56))),
            ..Default::default()
        },
        Animation { shape: id, class: AnimClass::Exit, effect: "shrinkTurn".into(), ..Default::default() },
        Animation { shape: id, class: AnimClass::Exit, effect: "floatOut".into(), option: "d".into(), ..Default::default() },
        Animation { shape: id, class: AnimClass::Exit, effect: "bounceOut".into(), ..Default::default() },
        Animation { shape: id, class: AnimClass::Emphasis, effect: "objectColor".into(), ..Default::default() },
    ];
    Arc::make_mut(&mut p.slides[0]).animations = anims.clone();
    let back = deckcraft_odp::import(&deckcraft_odp::export(&p).unwrap()).unwrap();
    let got = &back.slides[0].animations;
    assert_eq!(got.len(), anims.len());
    for (want, got) in anims.iter().zip(got) {
        let key = |a: &Animation| (a.class, a.effect.clone(), a.option.clone(), a.repeat, a.auto_reverse, a.rewind, a.amount);
        assert_eq!(key(got), key(want), "{}", want.effect);
        assert_eq!((got.smooth_start, got.smooth_end), (want.smooth_start, want.smooth_end), "{}", want.effect);
    }
    assert_eq!(got[4].color, anims[4].color);
}

/// Each transition comes back as the kind and direction it was written with.
#[test]
fn transition_directions_round_trip() {
    for (kind, option) in [
        ("uncover", "l"),
        ("cover", "ru"),
        ("wipe", "l"),
        ("split", "horzIn"),
        ("clock", "counterClockwise"),
        ("fade", "throughBlack"),
        ("push", "r"),
    ] {
        let mut p = deck_with(vec![]);
        Arc::make_mut(&mut p.slides[0]).transition =
            Some(deckcraft_model::Transition { kind: kind.into(), option: option.into(), duration_ms: 900, ..Default::default() });
        let back = deckcraft_odp::import(&deckcraft_odp::export(&p).unwrap()).unwrap();
        let t = back.slides[0].transition.as_ref().unwrap();
        assert_eq!((t.kind.as_str(), t.option.as_str(), t.duration_ms), (kind, option, 900));
    }
}

/// A language with a script and a region keeps all three (`fo:language`, `fo:script`, `fo:country`).
#[test]
fn language_tags_round_trip() {
    let mut run = Run::new("文字");
    run.props.lang = Some("zh-Hans-CN".into());
    let shape = Shape {
        xfrm: Some(Xfrm::new(50.0, 50.0, 200.0, 50.0)),
        text_box: true,
        text: Some(TextBody { paragraphs: vec![Paragraph { runs: vec![run], ..Default::default() }], ..Default::default() }),
        ..Default::default()
    };
    let p = deck_with(vec![shape]);
    let back = deckcraft_odp::import(&deckcraft_odp::export(&p).unwrap()).unwrap();
    let t = shapes_of(&back)[0].text.clone().unwrap();
    assert_eq!(t.paragraphs[0].runs[0].props.lang.as_deref(), Some("zh-Hans-CN"));
}

#[test]
fn comments_round_trip() {
    let mut p = defaults::new_presentation(None);
    let c = deckcraft_model::Comment {
        author: "Ada".into(),
        initials: "AL".into(),
        text: "Check this\nsecond line".into(),
        date: "2026-11-04T10:00:00".into(),
        x: 100.0,
        y: 50.0,
        replies: vec![deckcraft_model::Comment { author: "Bo".into(), text: "Done".into(), ..Default::default() }],
        ..Default::default()
    };
    Arc::make_mut(&mut p.slides[0]).comments.push(c);
    let back = deckcraft_odp::import(&deckcraft_odp::export(&p).unwrap()).unwrap();
    let cs = &back.slides[0].comments;
    assert_eq!(cs.len(), 2);
    assert_eq!((cs[0].author.as_str(), cs[0].initials.as_str(), cs[0].text.as_str()), ("Ada", "AL", "Check this\nsecond line"));
    assert_eq!(cs[0].date, "2026-11-04T10:00:00");
    assert!((cs[0].x - 100.0).abs() < 0.01 && (cs[0].y - 50.0).abs() < 0.01);
    assert_eq!(cs[1].text, "Done");
}

#[test]
fn media_picture_bullets_columns_and_embedded_fonts_round_trip() {
    let mut p = deck_with(vec![]);
    let clip = p.add_media("talk.mp4", "video/mp4", b"\0\0\0\x18ftypmp42not-really-a-video".to_vec()).unwrap();
    let dot = p.add_media("dot.png", "image/png", vec![0x89, b'P', b'N', b'G', 1, 2, 3]).unwrap();
    let video = Shape {
        id: ShapeId(p.alloc_id().unwrap()),
        xfrm: Some(Xfrm::new(10.0, 10.0, 320.0, 180.0)),
        kind: ShapeKind::Media(deckcraft_model::MediaClip { media: clip, video: true, loop_play: true, volume: 0.5, ..Default::default() }),
        ..Default::default()
    };
    let mut para = Paragraph::new("Picture bullet");
    para.props.bullet = Some(Bullet::Picture { media: dot });
    let mut body = TextBody { paragraphs: vec![para], ..Default::default() };
    body.body.columns = Some(2);
    body.body.col_spacing = Some(18.0);
    let tb = Shape {
        id: ShapeId(p.alloc_id().unwrap()),
        xfrm: Some(Xfrm::new(10.0, 200.0, 400.0, 100.0)),
        text_box: true,
        text: Some(body),
        ..Default::default()
    };
    let font = deckcraft_fonts::bundled().into_iter().next().map(<[u8]>::to_vec);
    if let Some(f) = &font {
        let id = p.add_media("face.ttf", "application/x-font-ttf", f.clone()).unwrap();
        p.embedded_fonts.push(deckcraft_model::EmbeddedFont { typeface: "Deck Face".into(), regular: Some(id), ..Default::default() });
    }
    Arc::make_mut(&mut p.slides[0]).shapes.extend([video, tb]);
    let back = deckcraft_odp::import(&deckcraft_odp::export(&p).unwrap()).unwrap();
    let s = shapes_of(&back);
    let ShapeKind::Media(m) = &s[0].kind else { panic!("{:?}", s[0].kind) };
    assert!(m.video && m.loop_play && (m.volume - 0.5).abs() < 0.01, "{m:?}");
    assert_eq!(back.media(m.media).map(|i| i.data.to_vec()), Some(b"\0\0\0\x18ftypmp42not-really-a-video".to_vec()));
    let t = s[1].text.as_ref().unwrap();
    let Some(Bullet::Picture { media }) = t.paragraphs[0].props.bullet else { panic!("{:?}", t.paragraphs[0].props.bullet) };
    assert_eq!(back.media(media).map(|i| i.data.to_vec()), Some(vec![0x89, b'P', b'N', b'G', 1, 2, 3]));
    assert_eq!(t.body.columns, Some(2));
    assert!((t.body.col_spacing.unwrap() - 18.0).abs() < 0.01);
    if let Some(f) = font {
        let ef = back.embedded_fonts.iter().find(|e| e.typeface == "Deck Face").unwrap();
        assert_eq!(back.media(ef.regular.unwrap()).map(|i| i.data.to_vec()), Some(f));
    }
}

#[test]
fn glued_connectors_round_trip() {
    let a = Shape { xfrm: Some(Xfrm::new(50.0, 50.0, 100.0, 60.0)), geom: Geom::preset("rect"), ..Default::default() };
    let b = Shape { xfrm: Some(Xfrm::new(300.0, 200.0, 100.0, 60.0)), geom: Geom::preset("ellipse"), ..Default::default() };
    let mut p = deck_with(vec![a, b]);
    let ids: Vec<ShapeId> = shapes_of(&p).iter().map(|s| s.id).collect();
    let c = Shape {
        id: ShapeId(p.alloc_id().unwrap()),
        xfrm: Some(Xfrm::new(150.0, 80.0, 150.0, 120.0)),
        // The box's right site to the ellipse's left one (an ellipse's eight sites run
        // counter-clockwise from the top: its left is 2).
        kind: ShapeKind::Connector { start: Some((ids[0], 3)), end: Some((ids[1], 2)) },
        geom: Geom::preset("straightConnector1"),
        line: Some(deckcraft_model::style::Line::solid(ColorRef::rgb(Rgba::BLACK), 1.5)),
        ..Default::default()
    };
    Arc::make_mut(&mut p.slides[0]).shapes.push(c);
    let bytes = deckcraft_odp::export(&p).unwrap();
    // LibreOffice's glue points run top, right, bottom, left.
    let xml = part(&bytes, "content.xml");
    assert!(xml.contains(r#"draw:start-glue-point="1""#) && xml.contains(r#"draw:end-glue-point="3""#));
    let back = deckcraft_odp::import(&bytes).unwrap();
    let s = shapes_of(&back);
    let ShapeKind::Connector { start, end } = &s[2].kind else { panic!("{:?}", s[2].kind) };
    assert_eq!(*start, Some((s[0].id, 3)));
    assert_eq!(*end, Some((s[1].id, 2)));
    let x = s[2].xfrm.unwrap();
    assert!((x.x - 150.0).abs() < 0.01 && (x.w - 150.0).abs() < 0.01 && (x.h - 120.0).abs() < 0.01);
}

/// A bent connector stays a connector glued at both ends, along the same route.
#[test]
fn bent_connectors_keep_their_ends_and_route() {
    let a = Shape { xfrm: Some(Xfrm::new(50.0, 50.0, 100.0, 60.0)), geom: Geom::preset("rect"), ..Default::default() };
    let b = Shape { xfrm: Some(Xfrm::new(300.0, 200.0, 100.0, 60.0)), geom: Geom::preset("rect"), ..Default::default() };
    let mut p = deck_with(vec![a, b]);
    let ids: Vec<ShapeId> = shapes_of(&p).iter().map(|s| s.id).collect();
    let c = Shape {
        id: ShapeId(p.alloc_id().unwrap()),
        xfrm: Some(Xfrm::new(150.0, 80.0, 150.0, 120.0)),
        kind: ShapeKind::Connector { start: Some((ids[0], 3)), end: Some((ids[1], 1)) },
        geom: Geom::preset("bentConnector3"),
        line: Some(deckcraft_model::style::Line::solid(ColorRef::rgb(Rgba::BLACK), 1.5)),
        ..Default::default()
    };
    Arc::make_mut(&mut p.slides[0]).shapes.push(c);
    let bytes = deckcraft_odp::export(&p).unwrap();
    assert!(part(&bytes, "content.xml").contains(r#"draw:type="standard""#));
    let back = deckcraft_odp::import(&bytes).unwrap();
    let s = shapes_of(&back);
    let ShapeKind::Connector { start, end } = &s[2].kind else { panic!("{:?}", s[2].kind) };
    assert_eq!((*start, *end), (Some((s[0].id, 3)), Some((s[1].id, 1))));
    let x = s[2].xfrm.unwrap();
    assert!((x.x - 150.0).abs() < 0.5 && (x.y - 80.0).abs() < 0.5 && (x.w - 150.0).abs() < 0.5 && (x.h - 120.0).abs() < 0.5, "{x:?}");
}

/// A child filled with its group's fill, and a shape filled with the slide background, keep the
/// fill they show (ODF groups have no fill, and shapes no "background" fill).
#[test]
fn group_and_background_fills_are_written_as_they_show() {
    let red = Fill::solid(ColorRef::rgb(Rgba::rgb(200, 0, 0)));
    let green = Fill::solid(ColorRef::rgb(Rgba::rgb(0, 150, 0)));
    let child = Shape {
        id: ShapeId(8001),
        xfrm: Some(Xfrm::new(0.0, 0.0, 50.0, 50.0)),
        geom: Geom::preset("rect"),
        fill: Some(Fill::Group),
        ..Default::default()
    };
    let group = Shape {
        xfrm: Some(Xfrm::new(50.0, 50.0, 50.0, 50.0)),
        fill: Some(red.clone()),
        kind: ShapeKind::Group { children: vec![child], child: Xfrm::new(0.0, 0.0, 50.0, 50.0) },
        ..Default::default()
    };
    let knockout =
        Shape { xfrm: Some(Xfrm::new(200.0, 50.0, 50.0, 50.0)), geom: Geom::preset("rect"), fill: Some(Fill::Background), ..Default::default() };
    let mut p = deck_with(vec![group, knockout]);
    Arc::make_mut(&mut p.slides[0]).background = Some(deckcraft_model::Background::Fill { fill: green.clone() });
    let back = deckcraft_odp::import(&deckcraft_odp::export(&p).unwrap()).unwrap();
    let s = shapes_of(&back);
    let ShapeKind::Group { children, .. } = &s[0].kind else { panic!("{:?}", s[0].kind) };
    assert_eq!(children[0].fill, Some(red));
    assert_eq!(s[1].fill, Some(green));
}

/// What LibreOffice needs to keep our links, notes, starts and vertical text, and what it
/// writes back for them.
#[test]
fn links_notes_numbering_and_vertical_text_survive_libreoffice() {
    use deckcraft_model::text::{Action, Hyperlink, TextDir};
    let link =
        |action: Action| Hyperlink { action, tooltip: "tip".into(), highlight_click: false, sound: None, stop_sound: false, zoom_return: None };
    let mut web = Run::new("web");
    web.props.link = Some(link(Action::Url { url: "https://example.org".into() }));
    let mut next = Run::new("next");
    next.props.link = Some(link(Action::NextSlide));
    let mut numbered = Paragraph { runs: vec![Run::new("three")], ..Default::default() };
    numbered.props.bullet = Some(Bullet::AutoNum { scheme: "arabicPeriod".into(), start_at: 3 });
    let shape = Shape {
        xfrm: Some(Xfrm::new(50.0, 50.0, 300.0, 200.0)),
        text_box: true,
        text: Some(TextBody { paragraphs: vec![Paragraph { runs: vec![web, next], ..Default::default() }, numbered], ..Default::default() }),
        ..Default::default()
    };
    let mut vertical =
        Shape { xfrm: Some(Xfrm::new(400.0, 50.0, 100.0, 300.0)), text_box: true, text: Some(TextBody::from_text("up")), ..Default::default() };
    if let Some(t) = vertical.text.as_mut() {
        t.body.vert = Some(TextDir::Vertical270);
    }
    let mut p = deck_with(vec![shape, vertical]);
    Arc::make_mut(&mut p.slides[0]).notes = TextBody::from_text("say this");
    let bytes = deckcraft_odp::export(&p).unwrap();
    let xml = part(&bytes, "content.xml");
    // The link inside its span, with its tooltip; the jump as LibreOffice names it.
    assert!(xml.contains(r#"<text:a xlink:type="simple" xlink:href="https://example.org" office:title="tip">web</text:a></text:span>"#), "{xml}");
    assert!(xml.contains("#action?jump=nextslide"));
    assert!(xml.contains(r#"xmlns:dom="http://www.w3.org/2001/xml-events""#));
    assert!(xml.contains(r#"-notes" presentation:class="notes""#));
    assert!(xml.contains(r#"text:start-value="3">"#) && xml.contains(r#"loext:writing-mode="bt-lr""#));
    let back = deckcraft_odp::import(&bytes).unwrap();
    let s = shapes_of(&back);
    let runs = &s[0].text.as_ref().unwrap().paragraphs[0].runs;
    assert_eq!(runs[0].props.link.as_ref().map(|l| l.tooltip.as_str()), Some("tip"));
    assert_eq!(runs.iter().find(|r| r.text == "next").and_then(|r| r.props.link.as_ref()).map(|l| l.action.clone()), Some(Action::NextSlide));
    assert_eq!(s[1].text.as_ref().unwrap().body.vert, Some(TextDir::Vertical270));
    assert_eq!(back.slides[0].notes.text().trim(), "say this");
}

/// LibreOffice's own forms: connectors routed in page coordinates under a view box, cell
/// borders in paragraph properties, hidden shapes as `drawooo:display`, masters by their
/// escaped style name only.
#[test]
fn libreoffice_saved_forms_read() {
    let doc = format!(
        r#"<office:document {NS} xmlns:presentation="urn:oasis:names:tc:opendocument:xmlns:presentation:1.0" xmlns:drawooo="http://openoffice.org/2010/draw" office:mimetype="application/vnd.oasis.opendocument.presentation"><office:automatic-styles><style:style style:name="ce1" style:family="table-cell"><style:paragraph-properties fo:border="0.05cm solid #ff0000"/></style:style></office:automatic-styles><office:master-styles><style:master-page style:name="My_20_Master"/></office:master-styles><office:body><office:presentation><draw:page draw:master-page-name="My_20_Master">
<draw:connector draw:type="standard" svg:x1="10cm" svg:y1="5cm" svg:x2="17cm" svg:y2="8.5cm" svg:viewBox="0 0 7056 3528" svg:d="M10000 5000h3500v3500h3500"/>
<draw:frame svg:x="1cm" svg:y="10cm" svg:width="8cm" svg:height="2cm"><table:table><table:table-column/><table:table-row><table:table-cell table:style-name="ce1"><text:p>x</text:p></table:table-cell></table:table-row></table:table></draw:frame>
<draw:custom-shape drawooo:display="none" svg:x="1cm" svg:y="1cm" svg:width="1cm" svg:height="1cm"><draw:enhanced-geometry svg:viewBox="0 0 10 10" draw:enhanced-path="M 0 0 L 10 10 N"/></draw:custom-shape>
</draw:page></office:presentation></office:body></office:document>"#
    );
    let p = deckcraft_odp::import(doc.as_bytes()).unwrap();
    assert_eq!(p.masters[0].name, "My Master");
    let sh = &p.slides[0].shapes;
    let conn = sh.iter().find(|s| matches!(s.kind, ShapeKind::Connector { .. })).unwrap();
    let x = conn.xfrm.unwrap();
    assert!(x.w > 100.0 && x.h > 50.0, "{x:?}");
    let table = sh.iter().find_map(|s| if let ShapeKind::Table(t) = &s.kind { Some(t) } else { None }).unwrap();
    let left = table.rows[0].cells[0].borders[0].as_ref().unwrap();
    assert_eq!(left.fill, Some(Fill::solid(ColorRef::rgb(Rgba::rgb(255, 0, 0)))));
    assert!(sh.iter().any(|s| s.hidden));
}

/// What one definition costs is paid once, not once per use: a long footer on many pages, a
/// hatch on many shapes, many connectors, many namespace declarations, many custom shapes whose
/// formulas use their whole budget, and a list style with thousands of levels.
#[test]
fn shared_definitions_cost_once() {
    let ns = r#"xmlns:presentation="urn:oasis:names:tc:opendocument:xmlns:presentation:1.0""#;
    let flat = |styles: &str, body: String| {
        format!(
            r#"<office:document {NS} {ns} office:mimetype="application/vnd.oasis.opendocument.presentation"><office:styles>{styles}</office:styles><office:automatic-styles><style:style style:name="dp" style:family="drawing-page"><style:drawing-page-properties presentation:display-footer="true"/></style:style></office:automatic-styles><office:body><office:presentation>{body}</office:presentation></office:body></office:document>"#
        )
    };
    let time = |name: &str, doc: String| {
        let started = std::time::Instant::now();
        let p = deckcraft_odp::import(doc.as_bytes()).unwrap();
        assert!(started.elapsed().as_secs() < 20, "{name}: {:?}", started.elapsed());
        p
    };
    // A 1 MB footer on 300 pages keeps a short copy on each.
    let pages = r#"<draw:page draw:style-name="dp" presentation:use-footer-name="f"/>"#.repeat(300);
    let footer = format!(r#"<presentation:footer-decl presentation:name="f">{}</presentation:footer-decl>{pages}"#, "a".repeat(1 << 20));
    let p = time("footer", flat("", footer));
    assert!(p.header_footer.footer_text.len() <= 1024);
    // One hatch on 300 shapes is drawn once.
    let hatch = r#"<draw:hatch draw:name="h" draw:style="triple" draw:distance="150pt"/><style:style style:name="g" style:family="graphic"><style:graphic-properties draw:fill="hatch" draw:fill-hatch-name="h"/></style:style>"#;
    let rects = format!("<draw:page>{}</draw:page>", r#"<draw:rect draw:style-name="g" svg:width="1cm" svg:height="1cm"/>"#.repeat(300));
    let p = time("hatch", flat(hatch, rects));
    assert!(p.media.len() <= 2, "{}", p.media.len());
    // 20 000 connectors glued to one shape.
    let glue = format!(
        r#"<draw:page>{}<draw:rect xml:id="t" svg:width="1cm" svg:height="1cm"/></draw:page>"#,
        r#"<draw:connector draw:start-shape="t" draw:end-shape="t"/>"#.repeat(20_000)
    );
    time("glue", flat("", glue));
    // 300 custom shapes whose formulas are one long cycle.
    let eqs: String = (0..24).map(|i| format!(r#"<draw:equation draw:name="f{i}" draw:formula="?f{0}+?f{0}"/>"#, (i + 1) % 24)).collect();
    let shape = format!(
        r#"<draw:custom-shape svg:width="1cm" svg:height="1cm"><draw:enhanced-geometry svg:viewBox="0 0 10 10" draw:enhanced-path="M ?f0 0 L 10 10 N">{eqs}</draw:enhanced-geometry></draw:custom-shape>"#
    );
    time("formulas", flat("", format!("<draw:page>{}</draw:page>", shape.repeat(300))));
    // A list style of 20 000 levels, used by 5 000 paragraphs.
    let levels = r#"<text:list-level-style-bullet text:level="99"/>"#.repeat(20_000);
    let list = format!(
        r#"<style:style style:name="g" style:family="graphic"><style:graphic-properties><text:list-style>{levels}</text:list-style></style:graphic-properties></style:style>"#
    );
    let paras = format!(
        r#"<draw:page><draw:frame draw:style-name="g" svg:width="1cm" svg:height="1cm"><draw:text-box>{}</draw:text-box></draw:frame></draw:page>"#,
        "<text:p>x</text:p>".repeat(5_000)
    );
    time("list", flat(&list, paras));
    // 20 000 namespace declarations over 100 000 elements.
    let decls: String = (0..20_000).map(|i| format!(r#" xmlns:p{i}="urn:x""#)).collect();
    let many = format!(
        r#"<office:document {NS}{decls}><office:body><office:presentation><draw:page>{}</draw:page></office:presentation></office:body></office:document>"#,
        "<a/>".repeat(100_000)
    );
    time("namespaces", many);
}

/// A gradient fading out keeps its fade: each stop its own opacity, not one average.
#[test]
fn gradient_opacity_varies_along_the_gradient() {
    use deckcraft_color::ColorTransform;
    use deckcraft_model::style::{Gradient, GradientStop};
    let red = |a: Option<i32>| {
        let mut c = ColorRef::rgb(Rgba::rgb(255, 0, 0));
        c.mods.extend(a.map(ColorTransform::Alpha));
        c
    };
    let g = Gradient {
        stops: vec![GradientStop { pos: 0.0, color: red(None) }, GradientStop { pos: 1.0, color: red(Some(0)) }],
        shape: GradientShape::Linear { angle: 0.0, scaled: false },
        flip: None,
        rotate_with_shape: true,
        blend: Default::default(),
        tile: [0.0; 4],
    };
    let shape =
        Shape { xfrm: Some(Xfrm::new(50.0, 50.0, 200.0, 100.0)), geom: Geom::preset("rect"), fill: Some(Fill::Gradient(g)), ..Default::default() };
    let back = deckcraft_odp::import(&deckcraft_odp::export(&deck_with(vec![shape])).unwrap()).unwrap();
    let Some(Fill::Gradient(g)) = &shapes_of(&back)[0].fill else { panic!("{:?}", shapes_of(&back)[0].fill) };
    let (first, last) = (g.stops.first().unwrap(), g.stops.last().unwrap());
    assert!(first.color.alpha() > 0.9 && last.color.alpha() < 0.1, "{:?}", g.stops);
}

/// Glow and soft edges are written as LibreOffice's extensions and read back.
#[test]
fn glow_and_soft_edges_round_trip() {
    use deckcraft_model::style::{Effects, Glow};
    let glow = Glow { color: ColorRef::rgb(Rgba::rgb(255, 200, 0)), radius: 8.0 };
    let shape = Shape {
        xfrm: Some(Xfrm::new(50.0, 50.0, 100.0, 100.0)),
        geom: Geom::preset("ellipse"),
        effects: Some(Effects { glow: Some(glow), soft_edge: Some(4.0), ..Default::default() }),
        ..Default::default()
    };
    let back = deckcraft_odp::import(&deckcraft_odp::export(&deck_with(vec![shape])).unwrap()).unwrap();
    let e = shapes_of(&back)[0].effects.clone().unwrap();
    let g = e.glow.unwrap();
    assert!((g.radius - 8.0).abs() < 0.1 && g.color == ColorRef::rgb(Rgba::rgb(255, 200, 0)), "{g:?}");
    assert!((e.soft_edge.unwrap() - 4.0).abs() < 0.1);
}

/// A by-paragraph build is written as one effect per paragraph (what LibreOffice plays) and read
/// back as the one build.
#[test]
fn by_paragraph_builds_round_trip() {
    use deckcraft_model::anim::{Animation, TextBuild};
    let text = Shape {
        xfrm: Some(Xfrm::new(50.0, 50.0, 300.0, 200.0)),
        text_box: true,
        text: Some(TextBody::from_text("one\ntwo\nthree")),
        ..Default::default()
    };
    let mut p = deck_with(vec![text]);
    let id = shapes_of(&p)[0].id;
    let build = Animation { shape: id, effect: "fade".into(), text_build: TextBuild::ByParagraph, ..Default::default() };
    Arc::make_mut(&mut p.slides[0]).animations = vec![build.clone()];
    let bytes = deckcraft_odp::export(&p).unwrap();
    assert_eq!(part(&bytes, "content.xml").matches("presentation:group-id=").count(), 3);
    let back = deckcraft_odp::import(&bytes).unwrap();
    let a = &back.slides[0].animations;
    assert_eq!(a.len(), 1, "{a:?}");
    assert_eq!((a[0].text_build, a[0].paragraph, a[0].start), (TextBuild::ByParagraph, None, build.start));
}

/// Distributed lines, underlines under words only, a coloured text shadow, a long date format and
/// unnamed slides come back as they were written.
#[test]
fn text_details_dates_and_slide_names_round_trip() {
    use deckcraft_model::style::Shadow;
    use deckcraft_model::text::Align;
    let mut run = Run::new("words here");
    run.props.underline = Some("words".into());
    run.props.shadow = Some(Shadow {
        color: ColorRef::rgb(Rgba::rgb(255, 0, 0)),
        blur: 2.0,
        dist: 3.0,
        dir: 45.0,
        inner: false,
        sx: 1.0,
        sy: 1.0,
        kx: 0.0,
        ky: 0.0,
        align: String::new(),
        rotate_with_shape: true,
    });
    let mut para = Paragraph { runs: vec![run], ..Default::default() };
    para.props.align = Some(Align::Distributed);
    let shape = Shape {
        xfrm: Some(Xfrm::new(50.0, 50.0, 300.0, 100.0)),
        text_box: true,
        text: Some(TextBody { paragraphs: vec![para], ..Default::default() }),
        ..Default::default()
    };
    let mut p = deck_with(vec![shape]);
    Arc::make_mut(&mut p.slides[0]).name.clear();
    p.header_footer.date = true;
    p.header_footer.date_format = "datetime4".into();
    let back = deckcraft_odp::import(&deckcraft_odp::export(&p).unwrap()).unwrap();
    assert_eq!(back.slides[0].name, "");
    assert_eq!(back.header_footer.date_format, "datetime4");
    let para = &shapes_of(&back)[0].text.clone().unwrap().paragraphs[0];
    assert_eq!(para.props.align, Some(Align::Distributed));
    let r = &para.runs[0].props;
    assert_eq!(r.underline.as_deref(), Some("words"));
    let sh = r.shadow.clone().unwrap();
    assert_eq!(sh.color, ColorRef::rgb(Rgba::rgb(255, 0, 0)));
    assert!((sh.dist - 3.0).abs() < 0.05 && (sh.dir - 45.0).abs() < 0.5 && (sh.blur - 2.0).abs() < 0.05, "{sh:?}");
}

/// A picture in an ellipse is written as an ellipse filled with the picture (an ODF picture frame
/// is a box), and keeps its outline.
#[test]
fn pictures_keep_their_outline() {
    let png: Vec<u8> = vec![
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0x0d, 0x49, 0x48, 0x44, 0x52, 0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0, 0x1f, 0x15,
        0xc4, 0x89, 0, 0, 0, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0xf0, 0x1f, 0, 0x05, 0, 0x01, 0xff, 0x89, 0x99, 0x3d,
        0x1d, 0, 0, 0, 0, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];
    let mut p = deck_with(vec![]);
    let media = p.add_media("dot.png", "image/png", png).unwrap();
    let pic = Shape {
        id: ShapeId(p.alloc_id().unwrap()),
        xfrm: Some(Xfrm::new(50.0, 50.0, 200.0, 100.0)),
        geom: Geom::preset("ellipse"),
        kind: ShapeKind::Picture { fill: deckcraft_model::style::PictureFill { media, ..Default::default() } },
        ..Default::default()
    };
    Arc::make_mut(&mut p.slides[0]).shapes.push(pic);
    let bytes = deckcraft_odp::export(&p).unwrap();
    assert!(part(&bytes, "content.xml").contains(r#"draw:fill="bitmap""#));
    let back = deckcraft_odp::import(&bytes).unwrap();
    let s = shapes_of(&back);
    assert_eq!(s[0].geom.preset_name(), Some("ellipse"));
    assert!(matches!(s[0].fill, Some(Fill::Picture(_))), "{:?}", s[0].fill);
}

/// Review round 3: a language tag with two regions writes one `fo:country`; a group without a
/// fill gives its group-filled children none (not their style's); a one-paragraph build is an
/// effect on the whole shape; an adjusted picture in an ellipse stays a picture.
#[test]
fn round_three_fixes_hold() {
    use deckcraft_model::anim::{Animation, TextBuild};
    let mut run = Run::new("x");
    run.props.lang = Some("en-US-GB".into());
    let text = Shape {
        xfrm: Some(Xfrm::new(50.0, 50.0, 200.0, 50.0)),
        text_box: true,
        text: Some(TextBody { paragraphs: vec![Paragraph { runs: vec![run], ..Default::default() }], ..Default::default() }),
        ..Default::default()
    };
    let mut child = Shape {
        id: ShapeId(8101),
        xfrm: Some(Xfrm::new(0.0, 0.0, 50.0, 50.0)),
        geom: Geom::preset("rect"),
        fill: Some(Fill::Group),
        ..Default::default()
    };
    child.style = Some(deckcraft_model::style::ShapeStyle::accent(deckcraft_color::SchemeSlot::Accent1));
    let group = Shape {
        xfrm: Some(Xfrm::new(300.0, 50.0, 50.0, 50.0)),
        kind: ShapeKind::Group { children: vec![child], child: Xfrm::new(0.0, 0.0, 50.0, 50.0) },
        ..Default::default()
    };
    let mut p = deck_with(vec![text, group]);
    let id = shapes_of(&p)[0].id;
    Arc::make_mut(&mut p.slides[0]).animations = vec![Animation { shape: id, text_build: TextBuild::ByParagraph, ..Default::default() }];
    let bytes = deckcraft_odp::export(&p).unwrap();
    let xml = part(&bytes, "content.xml");
    for (tag, attrs) in tags(&xml) {
        let names: std::collections::HashSet<&String> = attrs.iter().map(|(k, _)| k).collect();
        assert_eq!(names.len(), attrs.len(), "<{tag}> repeats an attribute");
    }
    let back = deckcraft_odp::import(&bytes).unwrap();
    let s = shapes_of(&back);
    let ShapeKind::Group { children, .. } = &s[1].kind else { panic!("{:?}", s[1].kind) };
    assert!(matches!(children[0].fill, None | Some(Fill::None)), "{:?}", children[0].fill);
    let a = &back.slides[0].animations;
    assert_eq!((a.len(), a[0].paragraph), (1, None));
}

/// Review round 3, re-test: builds share the page's effect budget when expanded; glue to a long
/// custom path doesn't build the path per connector; hatches over many colours draw a bounded
/// number of tiles; table flags round-trip.
#[test]
fn round_three_retest_fixes_hold() {
    use deckcraft_model::anim::{Animation, TextBuild};
    // 300 paragraphs, 300 by-paragraph builds on them.
    let lines: Vec<String> = (0..300).map(|i| format!("line {i}")).collect();
    let text = Shape {
        xfrm: Some(Xfrm::new(50.0, 50.0, 300.0, 300.0)),
        text_box: true,
        text: Some(TextBody::from_text(&lines.join("\n"))),
        ..Default::default()
    };
    let mut p = deck_with(vec![text]);
    let id = shapes_of(&p)[0].id;
    Arc::make_mut(&mut p.slides[0]).animations =
        (0..300).map(|_| Animation { shape: id, text_build: TextBuild::ByParagraph, ..Default::default() }).collect();
    let started = std::time::Instant::now();
    let xml = part(&deckcraft_odp::export(&p).unwrap(), "content.xml");
    assert!(xml.matches("presentation:preset-class=").count() <= 2_000 + 300, "{}", xml.matches("presentation:preset-class=").count());
    assert!(started.elapsed().as_secs() < 20);

    let flat = |body: String| {
        format!(
            r#"<office:document {NS} office:mimetype="application/vnd.oasis.opendocument.presentation"><office:styles><draw:hatch draw:name="h" draw:style="triple" draw:distance="150pt"/></office:styles><office:body><office:presentation><draw:page>{body}</draw:page></office:presentation></office:body></office:document>"#
        )
    };
    // 2000 connectors glued to one long custom path.
    let d: String = (0..50_000).map(|i| format!(" L{} {}", i % 1000, i % 777)).collect();
    let glue = format!(
        r#"<draw:path xml:id="t" svg:width="10cm" svg:height="10cm" svg:viewBox="0 0 1000 1000" svg:d="M0 0{d}"/>{}"#,
        r#"<draw:connector draw:start-shape="t" draw:end-shape="t"/>"#.repeat(2000)
    );
    let started = std::time::Instant::now();
    deckcraft_odp::import(flat(glue).as_bytes()).unwrap();
    assert!(started.elapsed().as_secs() < 20, "{:?}", started.elapsed());
    // One hatch over 500 background colours.
    let styles: String = (0..500)
        .map(|i| format!(r##"<style:style style:name="g{i}" style:family="graphic"><style:graphic-properties draw:fill="hatch" draw:fill-hatch-name="h" draw:fill-hatch-solid="true" draw:fill-color="#{:06x}"/></style:style>"##, i * 97))
        .collect();
    let rects: String = (0..500).map(|i| format!(r#"<draw:rect draw:style-name="g{i}" svg:width="1cm" svg:height="1cm"/>"#)).collect();
    let doc = flat(rects).replace("</office:styles>", &format!("{styles}</office:styles>"));
    let started = std::time::Instant::now();
    let p = deckcraft_odp::import(doc.as_bytes()).unwrap();
    assert!(p.media.len() <= 64, "{}", p.media.len());
    assert!(started.elapsed().as_secs() < 20, "{:?}", started.elapsed());
}

/// Review round 4: what round-trips now — bubble domains, date and time fields, invisible text,
/// explicit "off" formatting, raised text size, comment white space, spaces after a line break,
/// single-colour pies, per-series labels, show settings, number schemes, kerning, meta.
#[test]
fn round_four_fixes_hold() {
    use deckcraft_model::chart::{Chart, ChartType, Series};
    use deckcraft_model::text::{Caps, RunKind};
    let field = |f: &str| Run { kind: RunKind::Field { field: f.into() }, ..Run::new("x") };
    let mut invisible = Run::new("ghost");
    invisible.props.fill = Some(Fill::solid(ColorRef::rgb(Rgba::rgb(255, 0, 0)).with(deckcraft_color::ColorTransform::Alpha(0))));
    let mut plain = Run::new("Mixed");
    (plain.props.caps, plain.props.spacing, plain.props.baseline, plain.props.kern) = (Some(Caps::None), Some(0.0), Some(0.0), Some(12.0));
    let mut sup = Run::new("2");
    (sup.props.baseline, sup.props.size) = (Some(0.3), Some(20.0));
    let mut minus = Paragraph { runs: vec![Run::new("n")], ..Default::default() };
    minus.props.bullet = Some(Bullet::AutoNum { scheme: "arabic2Minus".into(), start_at: 1 });
    let after_break = Paragraph { runs: vec![Run::new("a"), Run { kind: RunKind::Break, ..Run::new("") }, Run::new("  lead")], ..Default::default() };
    let body = TextBody {
        paragraphs: vec![
            Paragraph { runs: vec![field("datetime10"), field("datetime4")], ..Default::default() },
            Paragraph { runs: vec![invisible, plain, sup], ..Default::default() },
            minus,
            after_break,
        ],
        ..Default::default()
    };
    let text = Shape { xfrm: Some(Xfrm::new(20.0, 20.0, 400.0, 200.0)), text_box: true, text: Some(body), ..Default::default() };
    let bubble = Chart {
        kind: ChartType::Bubble,
        series: vec![Series {
            name: "B".into(),
            x_values: vec![Some(1.0), Some(2.0)],
            values: vec![Some(10.0), Some(20.0)],
            sizes: vec![Some(1.0), Some(5.0)],
            ..Default::default()
        }],
        ..Default::default()
    };
    let green = Fill::solid(ColorRef::rgb(Rgba::rgb(0, 150, 0)));
    let pie = Chart {
        kind: ChartType::Pie,
        vary_colors: false,
        series: vec![Series { name: "P".into(), values: vec![Some(1.0), Some(2.0), Some(3.0)], fill: Some(green), ..Default::default() }],
        categories: vec!["a".into(), "b".into(), "c".into()],
        ..Default::default()
    };
    let labels = Chart {
        kind: ChartType::Column,
        series: vec![
            Series { name: "S1".into(), values: vec![Some(1.0)], data_labels: Some(true), ..Default::default() },
            Series { name: "S2".into(), values: vec![Some(2.0)], ..Default::default() },
        ],
        categories: vec!["a".into()],
        ..Default::default()
    };
    let chart = |c: Chart, y: f64| Shape { xfrm: Some(Xfrm::new(20.0, y, 200.0, 100.0)), kind: ShapeKind::Chart(Box::new(c)), ..Default::default() };
    let mut p = deck_with(vec![text, chart(bubble, 250.0), chart(pie, 360.0), chart(labels, 470.0)]);
    Arc::make_mut(&mut p.slides[0]).comments.push(deckcraft_model::Comment {
        author: "A".into(),
        text: "col1\tcol2  two\n  indented".into(),
        ..Default::default()
    });
    (p.show.show_type, p.show.use_timings, p.show.without_animation) = ("browsed".into(), false, true);
    (p.props.revision, p.props.category, p.props.company) = (7, "Science".into(), "Aurora Ltd".into());
    let back = deckcraft_odp::import(&deckcraft_odp::export(&p).unwrap()).unwrap();
    let s = shapes_of(&back);
    let paras = &s[0].text.as_ref().unwrap().paragraphs;
    let fields: Vec<String> =
        paras[0].runs.iter().filter_map(|r| if let RunKind::Field { field } = &r.kind { Some(field.clone()) } else { None }).collect();
    assert_eq!(fields, ["datetime10", "datetime4"]);
    let r = &paras[1].runs;
    assert!(r[0].props.fill.as_ref().is_some_and(|f| matches!(f, Fill::Solid { color } if color.alpha() < 0.01)), "{:?}", r[0].props.fill);
    let m = &r.iter().find(|x| x.text == "Mixed").unwrap().props;
    assert_eq!((m.caps, m.spacing, m.baseline), (Some(Caps::None), Some(0.0), Some(0.0)));
    assert!(m.kern.is_some_and(|k| k > 0.0));
    let sp = &r.iter().find(|x| x.text == "2").unwrap().props;
    assert!(sp.size.is_some_and(|z| (z - 20.0).abs() < 0.2), "{:?}", sp.size);
    // Arabic letters with a dash: ODF has no such format, so they come back as `1.`.
    assert!(matches!(&paras[2].props.bullet, Some(Bullet::AutoNum { scheme, .. }) if scheme == "arabicPeriod"));
    assert_eq!(paras[3].text(), "a\u{b}  lead");
    let charts: Vec<&Chart> = s.iter().filter_map(|x| if let ShapeKind::Chart(c) = &x.kind { Some(&**c) } else { None }).collect();
    assert_eq!(
        (charts[0].series[0].x_values.clone(), charts[0].series[0].values.clone()),
        (vec![Some(1.0), Some(2.0)], vec![Some(10.0), Some(20.0)])
    );
    assert!(!charts[1].vary_colors);
    assert_eq!((charts[2].series[0].data_labels, charts[2].series[1].data_labels.unwrap_or(false)), (Some(true), false));
    assert_eq!(back.slides[0].comments[0].text, "col1\tcol2  two\n  indented");
    assert_eq!((back.show.show_type.as_str(), back.show.use_timings, back.show.without_animation), ("browsed", false, true));
    assert_eq!((back.props.revision, back.props.category.as_str(), back.props.company.as_str()), (7, "Science", "Aurora Ltd"));
}

/// Fuzzing, round 4: many big tables share one document budget of cells, and huge coordinates
/// are written short enough to read back.
#[test]
fn tables_share_a_budget_and_huge_coordinates_round_trip() {
    let table = r#"<draw:frame svg:width="10cm" svg:height="5cm"><table:table><table:table-row table:number-rows-repeated="1000"><table:table-cell table:number-columns-repeated="1000"/></table:table-row></table:table></draw:frame>"#;
    let doc = |body: String| {
        format!(
            r#"<office:document {NS} office:mimetype="application/vnd.oasis.opendocument.presentation"><office:body><office:presentation><draw:page>{body}</draw:page></office:presentation></office:body></office:document>"#
        )
    };
    let p = deckcraft_odp::import(doc(table.repeat(10)).as_bytes()).unwrap();
    let mut cells = 0usize;
    deckcraft_model::walk(&p.slides[0].shapes, &mut |sh, _| {
        if let ShapeKind::Table(t) = &sh.kind {
            cells += t.rows.iter().map(|r| r.cells.len()).sum::<usize>();
        }
    });
    assert!(cells <= 262_144 + 10_000, "{cells}");
    // Arcs with radii of 1e308.
    let arcs = format!(
        r#"<draw:custom-shape svg:width="1cm" svg:height="1cm"><draw:enhanced-geometry svg:viewBox="0 0 10 10" draw:enhanced-path="{}N"/></draw:custom-shape>"#,
        "U 0 0 1e308 1e308 0 360 ".repeat(2000)
    );
    let p = deckcraft_odp::import(doc(arcs).as_bytes()).unwrap();
    let bytes = deckcraft_odp::export(&p).unwrap();
    assert!(part(&bytes, "content.xml").len() < 20 << 20);
    deckcraft_odp::import(&bytes).unwrap();
}

/// Review, round 5: a single-colour pie recolouring one slice stays single-colour, a scatter series with a
/// line colour stays unjoined, caps write both attributes (overriding a parent's other one), and a
/// restart on a deeper level leaves the outer numbering going.
#[test]
fn round_five_fixes_hold() {
    use deckcraft_model::chart::{Chart, ChartType, Series};
    use deckcraft_model::text::Caps;
    let red = Fill::solid(ColorRef::rgb(Rgba::rgb(200, 0, 0)));
    let pie = Chart {
        kind: ChartType::Pie,
        vary_colors: false,
        series: vec![Series {
            name: "P".into(),
            values: vec![Some(1.0), Some(2.0), Some(3.0), Some(4.0)],
            fill: Some(Fill::solid(ColorRef::rgb(Rgba::rgb(0, 150, 0)))),
            point_fills: vec![(1, red.clone())],
            ..Default::default()
        }],
        categories: vec!["a".into(), "b".into(), "c".into(), "d".into()],
        ..Default::default()
    };
    let scatter = Chart {
        kind: ChartType::Scatter,
        series: vec![Series {
            name: "S".into(),
            x_values: vec![Some(1.0), Some(2.0)],
            values: vec![Some(1.0), Some(4.0)],
            line: Some(deckcraft_model::Line { fill: Some(red), ..Default::default() }),
            ..Default::default()
        }],
        ..Default::default()
    };
    let chart = |c: Chart, y: f64| Shape { xfrm: Some(Xfrm::new(20.0, y, 200.0, 100.0)), kind: ShapeKind::Chart(Box::new(c)), ..Default::default() };
    let mut small = Run::new("small");
    small.props.caps = Some(Caps::Small);
    let mut all = Run::new("all");
    all.props.caps = Some(Caps::All);
    let num = |lvl: u8, start_at: u32, text: &str| {
        let mut p = Paragraph { runs: vec![Run::new(text)], ..Default::default() };
        (p.level, p.props.bullet) = (lvl, Some(Bullet::AutoNum { scheme: "arabicPeriod".into(), start_at }));
        p
    };
    let body = TextBody {
        paragraphs: vec![Paragraph { runs: vec![small, all], ..Default::default() }, num(0, 1, "one"), num(1, 1, "sub"), num(0, 1, "two")],
        ..Default::default()
    };
    let text = Shape { xfrm: Some(Xfrm::new(20.0, 300.0, 400.0, 200.0)), text_box: true, text: Some(body), ..Default::default() };
    let p = deck_with(vec![chart(pie, 20.0), chart(scatter, 150.0), text]);
    let bytes = deckcraft_odp::export(&p).unwrap();
    let content = part(&bytes, "content.xml");
    assert!(content.contains(r#"fo:font-variant="small-caps" fo:text-transform="none""#));
    assert!(content.contains(r#"fo:font-variant="normal" fo:text-transform="uppercase""#));
    let scatter = &part(&bytes, "Object 2/content.xml");
    // The series' own graphic properties: its line colour, and still no line.
    let at = scatter.find(r##"svg:stroke-color="#c80000""##).expect("line colour");
    let props = &scatter[scatter[..at].rfind('<').unwrap()..at + scatter[at..].find('>').unwrap()];
    assert!(props.contains(r#"draw:stroke="none""#), "{props}");
    // The restart on level 2 is on its own list, not the outer one.
    assert!(content.contains(r#"<text:list text:continue-numbering="false">"#), "{content}");
    let back = deckcraft_odp::import(&bytes).unwrap();
    let s = shapes_of(&back);
    let charts: Vec<&Chart> = s.iter().filter_map(|x| if let ShapeKind::Chart(c) = &x.kind { Some(&**c) } else { None }).collect();
    assert!(!charts[0].vary_colors);
    assert_eq!(charts[0].series[0].point_fills.iter().map(|(i, _)| *i).collect::<Vec<_>>(), [1]);
    assert_eq!(charts[1].kind, ChartType::Scatter);
    let paras = &s.iter().find_map(|x| x.text.as_ref()).unwrap().paragraphs;
    let caps: Vec<Option<Caps>> = paras[0].runs.iter().map(|r| r.props.caps).collect();
    assert_eq!(caps, [Some(Caps::Small), Some(Caps::All)]);
}

/// Review, round 6: date styles LibreOffice shows (in content.xml, times as time styles), WAV
/// under the type LibreOffice plays, tables that keep their banding through LibreOffice, footer
/// white space, a space before a line break, and a single-colour pie's slices.
#[test]
fn round_six_fixes_hold() {
    use deckcraft_model::chart::{Chart, ChartType, Series};
    use deckcraft_model::text::RunKind;
    let mut p = sample();
    let field = |f: &str| Run { kind: RunKind::Field { field: f.into() }, ..Run::new("x") };
    let body = TextBody {
        paragraphs: vec![
            Paragraph { runs: vec![field("datetime2"), field("datetime12")], ..Default::default() },
            Paragraph { runs: vec![Run::new("a b \nc")], ..Default::default() },
        ],
        ..Default::default()
    };
    let text = Shape { xfrm: Some(Xfrm::new(20.0, 20.0, 400.0, 100.0)), text_box: true, text: Some(body), ..Default::default() };
    let green = Fill::solid(ColorRef::rgb(Rgba::rgb(0, 150, 0)));
    let pie = Chart {
        kind: ChartType::Pie,
        vary_colors: false,
        series: vec![Series {
            name: "P".into(),
            values: vec![Some(1.0), Some(2.0), Some(3.0)],
            fill: Some(green.clone()),
            point_fills: vec![(1, Fill::solid(ColorRef::rgb(Rgba::rgb(200, 0, 0)))), (2, Fill::solid(ColorRef::rgb(Rgba::rgb(0, 0, 200))))],
            ..Default::default()
        }],
        categories: vec!["a".into(), "b".into(), "c".into()],
        ..Default::default()
    };
    let chart = Shape { xfrm: Some(Xfrm::new(20.0, 150.0, 200.0, 100.0)), kind: ShapeKind::Chart(Box::new(pie)), ..Default::default() };
    let wav = p.add_media("beep.wav", "audio/wav", vec![b'R', b'I', b'F', b'F']).unwrap();
    let clip = Shape {
        xfrm: Some(Xfrm::new(300.0, 150.0, 50.0, 50.0)),
        kind: ShapeKind::Media(deckcraft_model::MediaClip { media: wav, ..Default::default() }),
        ..Default::default()
    };
    let mut t = Table::new(2, 2, 200.0, 60.0);
    (t.first_row, t.band_row, t.first_col) = (false, false, true);
    let table = Shape { xfrm: Some(Xfrm::new(20.0, 300.0, 200.0, 60.0)), kind: ShapeKind::Table(t), ..Default::default() };
    let s = Arc::make_mut(&mut p.slides[0]);
    let ph = |kind: PhType| Shape {
        xfrm: Some(Xfrm::new(20.0, 400.0, 100.0, 20.0)),
        ph: Some(deckcraft_model::Placeholder { kind, ..Default::default() }),
        ..Default::default()
    };
    s.shapes = vec![text, chart, clip, table, ph(PhType::Footer), ph(PhType::Date)];
    let hf = &mut p.header_footer;
    (hf.footer, hf.footer_text, hf.date, hf.date_text) = (true, "A  B\tC".into(), true, "  fixed  date".into());
    let bytes = deckcraft_odp::export(&p).unwrap();
    let content = part(&bytes, "content.xml");
    assert!(content.contains(r#"<number:time-style style:name="datetime12">"#), "{content}");
    assert!(content.contains(r#"draw:mime-type="audio/x-wav""#));
    assert!(content.contains(r#"<table:table table:template-name="default" table:use-first-column-styles="true">"#));
    let back = deckcraft_odp::import(&bytes).unwrap();
    assert_eq!((back.header_footer.footer_text.as_str(), back.header_footer.date_text.as_str()), ("A  B\tC", "  fixed  date"));
    let s = shapes_of(&back);
    let paras = &s.iter().find_map(|x| x.text.as_ref()).unwrap().paragraphs;
    let fields: Vec<String> =
        paras[0].runs.iter().filter_map(|r| if let RunKind::Field { field } = &r.kind { Some(field.clone()) } else { None }).collect();
    assert_eq!(fields, ["datetime2", "datetime12"]);
    assert_eq!(paras[1].text(), "a b \u{b}c");
    // Read back varied or not, every slice keeps its colour: slice 0 the series' green.
    let c = s.iter().find_map(|x| if let ShapeKind::Chart(c) = &x.kind { Some(c) } else { None }).unwrap();
    let s0 = &c.series[0];
    let slice0 = s0.point_fills.iter().find(|(i, _)| *i == 0).map(|(_, f)| f).or(if c.vary_colors { None } else { s0.fill.as_ref() });
    assert_eq!(slice0, Some(&green));
    let clip = s.iter().find_map(|x| if let ShapeKind::Media(m) = &x.kind { Some(m) } else { None }).unwrap();
    assert!(!clip.video);
    let t = s.iter().find_map(|x| if let ShapeKind::Table(t) = &x.kind { Some(t) } else { None }).unwrap();
    assert_eq!((t.first_row, t.band_row, t.first_col), (false, false, true));
}

/// Review series 2, round 1: an opacity gradient fades a radial gradient from the centre out
/// (the opacity is laid on the stops before they're reshaped), and gradient stops don't multiply
/// through saves.
#[test]
fn gradient_opacity_and_stops_survive_saves() {
    use deckcraft_color::ColorTransform;
    use deckcraft_model::style::{Gradient, GradientStop};
    let c = |rgb: Rgba, alpha: Option<i32>| {
        let mut c = ColorRef::rgb(rgb);
        c.mods.extend(alpha.map(ColorTransform::Alpha));
        c
    };
    let (red, blue) = (Rgba::rgb(255, 0, 0), Rgba::rgb(0, 0, 255));
    // DrawingML order: position 0 at the centre, opaque; the edge transparent.
    let radial = Gradient {
        stops: vec![GradientStop { pos: 0.0, color: c(red, None) }, GradientStop { pos: 1.0, color: c(red, Some(0)) }],
        shape: GradientShape::Path { path: "circle".into(), focus: [0.5, 0.5, 0.5, 0.5] },
        flip: None,
        rotate_with_shape: true,
        blend: Default::default(),
        tile: [0.0; 4],
    };
    let linear = Gradient {
        stops: vec![GradientStop { pos: 0.0, color: c(red, None) }, GradientStop { pos: 1.0, color: c(blue, None) }],
        shape: GradientShape::Linear { angle: 0.0, scaled: false },
        flip: None,
        rotate_with_shape: true,
        blend: Default::default(),
        tile: [0.0; 4],
    };
    let shape = |g: Gradient, y: f64| Shape {
        xfrm: Some(Xfrm::new(50.0, y, 200.0, 100.0)),
        geom: Geom::preset("rect"),
        fill: Some(Fill::Gradient(g)),
        ..Default::default()
    };
    let mut p = deck_with(vec![shape(radial, 20.0), shape(linear, 200.0)]);
    let mut counts = vec![];
    for _ in 0..3 {
        p = deckcraft_odp::import(&deckcraft_odp::export(&p).unwrap()).unwrap();
        let s = shapes_of(&p);
        let Some(Fill::Gradient(g)) = &s[0].fill else { panic!("{:?}", s[0].fill) };
        let (centre, edge) = (g.stops.first().unwrap(), g.stops.last().unwrap());
        assert!(centre.color.alpha() > 0.9 && edge.color.alpha() < 0.1, "{:?}", g.stops);
        let Some(Fill::Gradient(g)) = &s[1].fill else { panic!("{:?}", s[1].fill) };
        counts.push(g.stops.len());
    }
    // A two-colour gradient on PowerPoint's curve is written as its ramp's stops (LibreOffice
    // blends in sRGB, so it draws the same), and read back as the two stops and the curve it was.
    assert_eq!(counts, [2, 2, 2]);
    let Some(Fill::Gradient(g)) = &shapes_of(&p)[1].fill else { panic!() };
    assert_eq!(g.blend, deckcraft_model::style::GradientBlend::PowerPoint);
    let ramp = deckcraft_render::gradient_ramp(&[(0.0, red), (1.0, blue)], deckcraft_model::style::GradientBlend::PowerPoint);
    // (Gradients are named styles, in styles.xml.)
    let styles = part(&deckcraft_odp::export(&p).unwrap(), "styles.xml");
    assert!(styles.matches("<loext:gradient-stop ").count() >= ramp.len(), "{styles}");
    // An sRGB two-stop gradient saved as `.pptx` repeats its first stop (PowerPoint draws two on a
    // curve, more straight).
    let srgb = Gradient {
        stops: vec![GradientStop { pos: 0.0, color: c(red, None) }, GradientStop { pos: 1.0, color: c(blue, None) }],
        shape: GradientShape::Linear { angle: 0.0, scaled: false },
        flip: None,
        rotate_with_shape: true,
        tile: [0.0; 4],
        blend: deckcraft_model::style::GradientBlend::Srgb,
    };
    let p = deck_with(vec![shape(srgb, 20.0)]);
    let pptx = deckcraft_pptx::export(&p).unwrap();
    let mut z = zip::ZipArchive::new(std::io::Cursor::new(pptx.clone())).unwrap();
    let mut slide = String::new();
    std::io::Read::read_to_string(&mut z.by_name("ppt/slides/slide1.xml").unwrap(), &mut slide).unwrap();
    assert_eq!(slide.matches("<a:gs ").count(), 3, "{slide}");
    // And reads back as the two stops blending in sRGB it was.
    let back = deckcraft_pptx::import(&pptx).unwrap();
    let Some(Fill::Gradient(g)) = &shapes_of(&back)[0].fill else { panic!() };
    assert_eq!((g.stops.len(), g.blend), (2, deckcraft_model::style::GradientBlend::Srgb));
}

/// Review series 2, round 1: slide date placeholders keep their format, a lone varied series
/// keeps its colours, audio of any type stays audio, a hatch keeps its opacity, and a text box
/// that doesn't wrap stays unwrapped.
#[test]
fn series_two_round_one_fixes_hold() {
    use deckcraft_model::chart::{Chart, ChartType, Series};
    use deckcraft_model::style::PatternFill;
    use deckcraft_model::text::RunKind;
    let mut p = sample();
    let mut date = Shape {
        xfrm: Some(Xfrm::new(20.0, 400.0, 100.0, 20.0)),
        ph: Some(deckcraft_model::Placeholder { kind: PhType::Date, ..Default::default() }),
        text: Some(TextBody::default()),
        ..Default::default()
    };
    date.text.as_mut().unwrap().paragraphs =
        vec![Paragraph { runs: vec![Run { kind: RunKind::Field { field: "datetime12".into() }, ..Run::new("9:30 AM") }], ..Default::default() }];
    let column = Chart {
        kind: ChartType::Column,
        vary_colors: true,
        series: vec![Series { name: "S".into(), values: vec![Some(1.0), Some(2.0), Some(3.0)], ..Default::default() }],
        categories: vec!["a".into(), "b".into(), "c".into()],
        ..Default::default()
    };
    let chart = Shape { xfrm: Some(Xfrm::new(20.0, 20.0, 200.0, 100.0)), kind: ShapeKind::Chart(Box::new(column)), ..Default::default() };
    let aiff = p.add_media("sound", "audio/x-aiff", vec![b'F', b'O', b'R', b'M']).unwrap();
    let clip = Shape {
        xfrm: Some(Xfrm::new(300.0, 20.0, 50.0, 50.0)),
        kind: ShapeKind::Media(deckcraft_model::MediaClip { media: aiff, ..Default::default() }),
        ..Default::default()
    };
    let half = |c: Rgba| ColorRef::rgb(c).with(deckcraft_color::ColorTransform::Alpha(50_000));
    let hatched = Shape {
        xfrm: Some(Xfrm::new(20.0, 150.0, 100.0, 100.0)),
        geom: Geom::preset("rect"),
        fill: Some(Fill::Pattern(PatternFill { preset: "ltUpDiag".into(), fg: half(Rgba::rgb(0, 0, 200)), bg: half(Rgba::WHITE) })),
        ..Default::default()
    };
    let mut unwrapped = Shape {
        xfrm: Some(Xfrm::new(150.0, 150.0, 100.0, 30.0)),
        text_box: true,
        text: Some(TextBody::from_text("a long line")),
        ..Default::default()
    };
    unwrapped.text.as_mut().unwrap().body.wrap = Some(false);
    let s = Arc::make_mut(&mut p.slides[0]);
    s.shapes = vec![date, chart, clip, hatched, unwrapped];
    (p.header_footer.date, p.header_footer.date_format) = (true, "datetime12".into());
    let bytes = deckcraft_odp::export(&p).unwrap();
    let back = deckcraft_odp::import(&bytes).unwrap();
    let s = shapes_of(&back);
    let date = back.slides[0].shapes.iter().find(|x| x.ph_type() == Some(PhType::Date)).unwrap();
    let field = date.text.as_ref().unwrap().paragraphs.iter().flat_map(|p| &p.runs).find_map(|r| match &r.kind {
        RunKind::Field { field } => Some(field.clone()),
        _ => None,
    });
    assert_eq!(field.as_deref(), Some("datetime12"));
    let c = s.iter().find_map(|x| if let ShapeKind::Chart(c) = &x.kind { Some(c) } else { None }).unwrap();
    assert!(c.vary_colors);
    assert_eq!(c.series[0].point_fills.len(), 3, "{:?}", c.series[0].point_fills);
    let clip = s.iter().find_map(|x| if let ShapeKind::Media(m) = &x.kind { Some(m) } else { None }).unwrap();
    assert!(!clip.video);
    assert!(part(&bytes, "content.xml").contains(r#"draw:fill="hatch""#));
    let hatch = s.iter().find(|x| matches!(x.fill, Some(Fill::Picture(_) | Fill::Pattern(_)))).unwrap();
    let alpha = match &hatch.fill {
        Some(Fill::Picture(pf)) => pf.alpha,
        Some(Fill::Pattern(pat)) => Some(pat.fg.alpha()),
        f => panic!("{f:?}"),
    };
    assert!(alpha.is_some_and(|a| (a - 0.5).abs() < 0.02), "{alpha:?}");
    let box_ = s.iter().find(|x| x.text_box).unwrap();
    assert_eq!(box_.text.as_ref().unwrap().body.wrap, Some(false));
}

/// Fuzzing, review series 2: a date style with a huge separator used by thousands of fields reads
/// at once, and copies of repeated table cells share one document budget of text.
#[test]
fn date_styles_and_copied_cell_text_are_bounded() {
    let doc = |styles: &str, body: String| {
        format!(
            r#"<office:document {NS} xmlns:number="urn:oasis:names:tc:opendocument:xmlns:datastyle:1.0" office:mimetype="application/vnd.oasis.opendocument.presentation"><office:automatic-styles>{styles}</office:automatic-styles><office:body><office:presentation><draw:page>{body}</draw:page></office:presentation></office:body></office:document>"#
        )
    };
    let style = format!(r#"<number:date-style style:name="D1"><number:text>{}</number:text></number:date-style>"#, "x".repeat(1 << 20));
    let fields = format!(
        r#"<draw:frame svg:width="10cm" svg:height="5cm"><draw:text-box><text:p>{}</text:p></draw:text-box></draw:frame>"#,
        r#"<text:date style:data-style-name="D1"/>"#.repeat(4000)
    );
    let started = std::time::Instant::now();
    deckcraft_odp::import(doc(&style, fields).as_bytes()).unwrap();
    assert!(started.elapsed().as_secs() < 5, "{:?}", started.elapsed());
    let table = format!(
        r#"<draw:frame svg:width="10cm" svg:height="5cm"><table:table><table:table-row><table:table-cell table:number-columns-repeated="1000"><text:p>{}</text:p></table:table-cell></table:table-row></table:table></draw:frame>"#,
        "y".repeat(4000)
    );
    let p = deckcraft_odp::import(doc("", table.repeat(260)).as_bytes()).unwrap();
    let mut chars = 0usize;
    deckcraft_model::walk(&p.slides[0].shapes, &mut |sh, _| {
        if let ShapeKind::Table(t) = &sh.kind {
            chars += t.rows.iter().flat_map(|r| &r.cells).map(|c| c.text.text().len()).sum::<usize>();
        }
    });
    // The copies' budget, and each table's own first cell.
    assert!(chars <= (4 << 20) + 260 * 4000, "{chars}");
}

/// Review series 2, round 2: master pages are how slides look and page layouts where their
/// placeholders go, so masters don't multiply through saves, their names don't grow, and every
/// slide keeps its layout (a section header, a title slide without a subtitle). A layout with a
/// look of its own gets a master page of its own, and that's stable too.
#[test]
fn masters_and_layouts_are_stable_through_saves() {
    let layouts_of = |p: &Presentation| -> Vec<(deckcraft_model::LayoutType, String)> {
        p.slides
            .iter()
            .map(|s| {
                let l = p.masters.iter().flat_map(|m| m.layouts.iter()).find(|l| l.id == s.layout).unwrap();
                (l.kind, l.name.clone())
            })
            .collect()
    };
    let masters_of = |p: &Presentation| p.masters.iter().map(|m| m.name.clone()).collect::<Vec<_>>();
    let mut p = sample();
    let first = layouts_of(&p);
    assert!(first.iter().any(|(k, _)| *k == deckcraft_model::LayoutType::SectionHeader));
    let mut back = p.clone();
    for _ in 0..2 {
        back = deckcraft_odp::import(&deckcraft_odp::export(&back).unwrap()).unwrap();
        assert_eq!(masters_of(&back), masters_of(&p));
        assert_eq!(layouts_of(&back), first);
    }
    // A layout with its own background: a master page of its own, then stable.
    let m = Arc::make_mut(&mut p.masters[0]);
    let id = p.slides[1].layout;
    let l = m.layouts.iter_mut().find(|l| l.id == id).unwrap();
    l.background = Some(deckcraft_model::Background::Fill { fill: Fill::solid(ColorRef::rgb(Rgba::rgb(10, 20, 30))) });
    let once = deckcraft_odp::import(&deckcraft_odp::export(&p).unwrap()).unwrap();
    let twice = deckcraft_odp::import(&deckcraft_odp::export(&once).unwrap()).unwrap();
    assert_eq!(once.masters.len(), 2);
    assert_eq!(masters_of(&twice), masters_of(&once));
    assert_eq!(layouts_of(&twice), layouts_of(&once));
}

/// Every line end, in every size, comes back as itself (written from one table, read back by its
/// path, the size as the nearest).
#[test]
fn line_ends_round_trip() {
    use deckcraft_model::style::{Line, LineEnd};
    let mut shapes = vec![];
    for kind in ["triangle", "oval", "diamond", "arrow", "stealth"] {
        for size in ["sm", "med", "lg"] {
            let end = LineEnd { kind: kind.into(), w: size.into(), len: size.into() };
            shapes.push(Shape {
                xfrm: Some(Xfrm::new(20.0, 20.0 + shapes.len() as f64 * 10.0, 200.0, 0.0)),
                geom: Geom::preset("line"),
                line: Some(Line { width: Some(2.0), fill: Some(Fill::solid(ColorRef::rgb(Rgba::BLACK))), tail: Some(end), ..Default::default() }),
                ..Default::default()
            });
        }
    }
    let want: Vec<_> = shapes.iter().map(|s| s.line.as_ref().and_then(|l| l.tail.clone())).collect();
    let back = deckcraft_odp::import(&deckcraft_odp::export(&deck_with(shapes)).unwrap()).unwrap();
    let got: Vec<_> = shapes_of(&back).iter().map(|s| s.line.as_ref().and_then(|l| l.tail.clone())).collect();
    assert_eq!(got, want);
}

/// A page layout reads back as a layout of its own: a custom layout keeps its kind, name and
/// placeholder boxes (it read as the master's first layout), and two layouts of one kind stay two.
#[test]
fn custom_layouts_keep_their_name_kind_and_boxes() {
    use deckcraft_model::{Layout, LayoutType, Placeholder};
    let mut p = sample();
    let m = Arc::make_mut(&mut p.masters[0]);
    let title = |x: f64| Shape {
        xfrm: Some(Xfrm::new(x, 40.0, 300.0, 60.0)),
        ph: Some(Placeholder { kind: PhType::Title, ..Default::default() }),
        text: Some(TextBody::default()),
        ..Default::default()
    };
    let custom = Layout {
        id: deckcraft_model::LayoutId(90_001),
        name: "My Layout".into(),
        kind: LayoutType::Custom,
        shapes: vec![title(123.0)],
        show_master_shapes: true,
        ..Default::default()
    };
    let mut second = m.layouts.iter().find(|l| l.kind == LayoutType::TitleOnly).unwrap().clone();
    (second.id, second.name) = (deckcraft_model::LayoutId(90_002), "Title Only Wide".into());
    second.shapes.retain(|s| s.ph_type() != Some(PhType::Title));
    second.shapes.push(title(7.0));
    m.layouts.push(custom);
    m.layouts.push(second);
    let s0 = Arc::make_mut(&mut p.slides[0]);
    s0.layout = deckcraft_model::LayoutId(90_001);
    let s1 = Arc::make_mut(&mut p.slides[1]);
    s1.layout = deckcraft_model::LayoutId(90_002);
    let back = deckcraft_odp::import(&deckcraft_odp::export(&p).unwrap()).unwrap();
    let layout = |i: usize| back.masters.iter().flat_map(|m| m.layouts.iter()).find(|l| l.id == back.slides[i].layout).unwrap();
    let title_x = |l: &Layout| l.shapes.iter().find(|s| s.ph_type().is_some_and(PhType::is_title)).and_then(|s| s.xfrm).map(|x| x.x.round());
    assert_eq!((layout(0).kind, layout(0).name.as_str(), title_x(layout(0))), (LayoutType::Custom, "My Layout", Some(123.0)));
    assert_eq!((layout(1).kind, layout(1).name.as_str(), title_x(layout(1))), (LayoutType::TitleOnly, "Title Only Wide", Some(7.0)));
    // A third save changes nothing.
    let again = deckcraft_odp::import(&deckcraft_odp::export(&back).unwrap()).unwrap();
    let names = |p: &Presentation| {
        p.slides.iter().map(|s| p.masters.iter().flat_map(|m| m.layouts.iter()).find(|l| l.id == s.layout).unwrap().name.clone()).collect::<Vec<_>>()
    };
    assert_eq!(names(&again), names(&back));
    assert_eq!(again.masters.iter().map(|m| m.layouts.len()).sum::<usize>(), back.masters.iter().map(|m| m.layouts.len()).sum::<usize>());
}

/// A varied chart is written so it reads back varied (its series has no colour of its own), and
/// the read model says what the renderer draws; a single-colour one recolouring a slice stays
/// single-colour. (Recolouring most of its slices it reads as varied: LibreOffice's varied pies
/// give their series a colour too, so a series colour can't mean "single colour".)
#[test]
fn varied_and_single_colour_charts_are_told_apart() {
    use deckcraft_model::chart::{Chart, ChartType, Series};
    let solid = |r, g, b| Fill::solid(ColorRef::rgb(Rgba::rgb(r, g, b)));
    let pie = |vary_colors: bool, fill: Option<Fill>, point_fills: Vec<(usize, Fill)>| Chart {
        kind: ChartType::Pie,
        vary_colors,
        series: vec![Series { name: "P".into(), values: vec![Some(1.0), Some(2.0), Some(3.0)], fill, point_fills, ..Default::default() }],
        categories: vec!["a".into(), "b".into(), "c".into()],
        ..Default::default()
    };
    let charts = [pie(false, Some(solid(0, 150, 0)), vec![(1, solid(200, 0, 0))]), pie(true, None, vec![])];
    let shapes = charts
        .iter()
        .enumerate()
        .map(|(i, c)| Shape {
            xfrm: Some(Xfrm::new(20.0, 20.0 + i as f64 * 120.0, 200.0, 100.0)),
            kind: ShapeKind::Chart(Box::new(c.clone())),
            ..Default::default()
        })
        .collect();
    let back = deckcraft_odp::import(&deckcraft_odp::export(&deck_with(shapes)).unwrap()).unwrap();
    let got: Vec<(bool, bool)> = shapes_of(&back)
        .iter()
        .filter_map(|s| if let ShapeKind::Chart(c) = &s.kind { Some((c.vary_colors, c.series[0].fill.is_some())) } else { None })
        .collect();
    assert_eq!(got, [(false, true), (true, false)]);
}

/// Review series 2, round 2 (LibreOffice files): a generic page layout picks the layout without
/// moving its boxes, masters no slide uses are kept, an opaque run under a translucent paragraph
/// style stays opaque through saves, and a flat opacity on a gradient reads exactly.
#[test]
fn libreoffice_layouts_masters_and_opacities_hold() {
    use deckcraft_model::LayoutType;
    let ns = format!(
        r#"{NS} xmlns:presentation="urn:oasis:names:tc:opendocument:xmlns:presentation:1.0" xmlns:loext="urn:org:documentfoundation:names:experimental:office:xmlns:loext:1.0""#
    );
    let doc = |styles: &str, page_attrs: &str, body: &str| {
        format!(
            r##"<office:document {ns} office:mimetype="application/vnd.oasis.opendocument.presentation"><office:styles>{styles}</office:styles><office:automatic-styles><style:style style:name="P1" style:family="paragraph"><style:text-properties fo:color="#666666" loext:opacity="30%"/></style:style><style:style style:name="T1" style:family="text"><style:text-properties fo:color="#666666" loext:opacity="100%"/></style:style><style:style style:name="gr1" style:family="graphic"><style:graphic-properties draw:fill="gradient" draw:fill-gradient-name="G" draw:opacity="50%"/></style:style></office:automatic-styles><office:master-styles><style:master-page style:name="M1"/><style:master-page style:name="M2"/></office:master-styles><office:body><office:presentation><draw:page draw:master-page-name="M1"{page_attrs}>{body}</draw:page></office:presentation></office:body></office:document>"##
        )
    };
    let gradient = r##"<draw:gradient draw:name="G" draw:style="linear" draw:start-color="#ff0000" draw:end-color="#0000ff"/>"##;
    let generic = r#"<style:presentation-page-layout style:name="AL1T0"><presentation:placeholder presentation:object="title" svg:x="1cm" svg:y="1cm" svg:width="-0.5cm" svg:height="1cm"/></style:presentation-page-layout>"#;
    let body = r#"<draw:frame presentation:class="title" svg:x="1cm" svg:y="1cm" svg:width="10cm" svg:height="2cm"><draw:text-box><text:p>Title</text:p></draw:text-box></draw:frame><draw:frame svg:x="1cm" svg:y="5cm" svg:width="10cm" svg:height="2cm"><draw:text-box><text:p text:style-name="P1"><text:span text:style-name="T1">opaque</text:span></text:p></draw:text-box></draw:frame><draw:custom-shape draw:style-name="gr1" svg:x="1cm" svg:y="9cm" svg:width="4cm" svg:height="2cm"><draw:enhanced-geometry svg:viewBox="0 0 21600 21600" draw:type="rectangle"/></draw:custom-shape>"#;
    let with = deckcraft_odp::import(doc(&format!("{gradient}{generic}"), r#" presentation:presentation-page-layout-name="AL1T0""#, body).as_bytes())
        .unwrap();
    let without = deckcraft_odp::import(doc(gradient, "", body).as_bytes()).unwrap();
    let title_only = |p: &Presentation| p.masters[0].layouts.iter().find(|l| l.kind == LayoutType::TitleOnly).unwrap().shapes.clone();
    assert_eq!(title_only(&with), title_only(&without));
    assert_eq!(with.masters.len(), 2);
    let saved = deckcraft_odp::import(&deckcraft_odp::export(&with).unwrap()).unwrap();
    assert_eq!(saved.masters.len(), 2, "{:?}", saved.masters.iter().map(|m| &m.name).collect::<Vec<_>>());
    let again = deckcraft_odp::import(&deckcraft_odp::export(&saved).unwrap()).unwrap();
    for p in [&with, &saved, &again] {
        let run = p.slides[0]
            .shapes
            .iter()
            .filter_map(|s| s.text.as_ref())
            .flat_map(|t| &t.paragraphs)
            .flat_map(|p| &p.runs)
            .find(|r| r.text == "opaque")
            .unwrap();
        let alpha = match &run.props.fill {
            Some(Fill::Solid { color }) => color.alpha(),
            f => panic!("{f:?}"),
        };
        assert!(alpha > 0.99, "{alpha}");
    }
    let shapes = shapes_of(&with);
    let Some(Fill::Gradient(g)) = &shapes.iter().find(|s| matches!(s.fill, Some(Fill::Gradient(_)))).unwrap().fill else { panic!() };
    assert_eq!(g.stops[0].color.mods, [deckcraft_color::ColorTransform::Alpha(50_000)]);
}

/// Review series 2, round 3: every layout is written (used or not) and read back as itself: two of
/// one kind and name stay two, one emptied of placeholders stays empty, an unused one survives;
/// and a file of thousands of page layouts makes a bounded number of layouts.
#[test]
fn every_layout_reads_back_as_itself_within_bounds() {
    use deckcraft_model::{LayoutId, LayoutType};
    let mut p = sample();
    let m = Arc::make_mut(&mut p.masters[0]);
    let only = m.layouts.iter().find(|l| l.kind == LayoutType::TitleOnly).unwrap().clone();
    let moved = |id: u32, x: f64| {
        let mut l = only.clone();
        (l.id, l.name) = (LayoutId(id), "Twin".into());
        for s in l.shapes.iter_mut().filter(|s| s.ph_type().is_some_and(PhType::is_title)) {
            s.xfrm = Some(Xfrm::new(x, 30.0, 200.0, 50.0));
        }
        l
    };
    let mut empty = only.clone();
    (empty.id, empty.name) = (LayoutId(90_103), "Empty".into());
    empty.shapes.retain(|s| s.ph_type().is_some_and(PhType::is_footer_kind));
    let mut unused = only.clone();
    (unused.id, unused.name) = (LayoutId(90_104), "Quote".into());
    m.layouts.extend([moved(90_101, 10.0), moved(90_102, 310.0), empty, unused]);
    for (i, id) in [(1, 90_101), (2, 90_102), (3, 90_103)] {
        Arc::make_mut(&mut p.slides[i]).layout = LayoutId(id);
    }
    let back = deckcraft_odp::import(&deckcraft_odp::export(&p).unwrap()).unwrap();
    let layout = |i: usize| back.masters[0].layouts.iter().find(|l| l.id == back.slides[i].layout).unwrap();
    let title_x = |i: usize| layout(i).shapes.iter().find(|s| s.ph_type().is_some_and(PhType::is_title)).and_then(|s| s.xfrm).map(|x| x.x.round());
    assert_ne!(back.slides[1].layout, back.slides[2].layout);
    assert_eq!((title_x(1), title_x(2)), (Some(10.0), Some(310.0)));
    assert!(layout(3).shapes.iter().all(|s| s.ph_type().is_some_and(PhType::is_footer_kind)), "{:?}", layout(3).shapes);
    assert!(back.masters[0].layouts.iter().any(|l| l.name == "Quote"));
    assert_eq!(back.masters[0].layouts.len(), p.masters[0].layouts.len());

    // Hostile: 1024 described page layouts of 64 placeholders, 10 masters, 10 000 pages each
    // naming another pair.
    let boxes = r#"<presentation:placeholder presentation:object="outline" svg:x="1cm" svg:y="1cm" svg:width="1cm" svg:height="1cm"/>"#.repeat(64);
    let layouts: String = (0..1024)
        .map(|i| format!(r#"<style:presentation-page-layout style:name="L{i}" style:display-name="L{i}">{boxes}</style:presentation-page-layout>"#))
        .collect();
    let masters: String = (0..10).map(|i| format!(r#"<style:master-page style:name="M{i}"/>"#)).collect();
    let pages: String = (0..10_000)
        .map(|i| format!(r#"<draw:page draw:master-page-name="M{}" presentation:presentation-page-layout-name="L{}"/>"#, i % 10, i % 1024))
        .collect();
    let doc = format!(
        r#"<office:document {NS} xmlns:presentation="urn:oasis:names:tc:opendocument:xmlns:presentation:1.0" office:mimetype="application/vnd.oasis.opendocument.presentation"><office:styles>{layouts}</office:styles><office:master-styles>{masters}</office:master-styles><office:body><office:presentation>{pages}</office:presentation></office:body></office:document>"#
    );
    let started = std::time::Instant::now();
    let p = deckcraft_odp::import(doc.as_bytes()).unwrap();
    let shapes: usize = p.masters.iter().flat_map(|m| &m.layouts).map(|l| l.shapes.len()).sum();
    // Each made layout: its 64 placeholders and the footers it keeps; and the masters' defaults.
    assert!(shapes <= 1024 * (64 + 3) + 10 * 11 * 16, "{shapes}");
    assert!(started.elapsed().as_secs() < 20, "{:?}", started.elapsed());
}

/// Review series 2, round 4: a master's layouts are its page layouts alone, in their order (not
/// the built defaults with ours claimed onto them), so a deleted layout stays deleted, a renamed
/// one is one layout, and the order holds; a layout with its own look is named for its own page,
/// so it isn't read twice.
#[test]
fn a_masters_layouts_are_its_page_layouts_alone() {
    use deckcraft_model::LayoutType;
    let mut p = sample();
    let m = Arc::make_mut(&mut p.masters[0]);
    let used: Vec<_> = p.slides.iter().map(|s| s.layout).collect();
    // Delete an unused layout, rename another, reverse the order.
    let gone = m.layouts.iter().position(|l| !used.contains(&l.id) && l.kind == LayoutType::Comparison).unwrap();
    m.layouts.remove(gone);
    let only = m.layouts.iter_mut().find(|l| l.kind == LayoutType::TitleOnly).unwrap();
    only.name = "Headline".into();
    m.layouts.reverse();
    // A layout with a look of its own that no slide uses.
    let mut look = m.layouts.iter().find(|l| l.kind == LayoutType::Blank).unwrap().clone();
    (look.id, look.name) = (deckcraft_model::LayoutId(90_201), "Dark".into());
    look.background = Some(deckcraft_model::Background::Fill { fill: Fill::solid(ColorRef::rgb(Rgba::rgb(5, 5, 5))) });
    m.layouts.push(look);
    let names = |p: &Presentation| p.masters.iter().map(|m| m.layouts.iter().map(|l| l.name.clone()).collect::<Vec<_>>()).collect::<Vec<_>>();
    let back = deckcraft_odp::import(&deckcraft_odp::export(&p).unwrap()).unwrap();
    let mut want = names(&p);
    let dark = want[0].pop().unwrap();
    assert_eq!(names(&back)[0], want[0]);
    // The look layout: on a master of its own (a known loss), once, with its background.
    let darks: Vec<_> = back.masters.iter().flat_map(|m| &m.layouts).filter(|l| l.name == dark).collect();
    assert_eq!(darks.len(), 1, "{:?}", names(&back));
    let dark_master = back.masters.iter().find(|m| m.layouts.iter().any(|l| l.name == dark)).unwrap();
    assert_eq!(dark_master.background, Some(deckcraft_model::Background::Fill { fill: Fill::solid(ColorRef::rgb(Rgba::rgb(5, 5, 5))) }));
    // And it holds.
    let again = deckcraft_odp::import(&deckcraft_odp::export(&back).unwrap()).unwrap();
    assert_eq!(names(&again), names(&back));
}

/// Every layout is written, so page layouts have room for many masters' (a cap shared with date
/// styles cut a hundred masters' last layouts), and a layout's placeholders keep their order
/// (footers had moved first).
#[test]
fn many_masters_keep_their_layouts_and_placeholder_order() {
    let mut p = sample();
    let first = p.masters[0].clone();
    for k in 1..100 {
        let mut m = (*first).clone();
        m.id = deckcraft_model::MasterId(80_000 + k);
        m.name = format!("Copy {k}");
        for (j, l) in m.layouts.iter_mut().enumerate() {
            l.id = deckcraft_model::LayoutId(100_000 + k * 100 + j as u32);
        }
        p.masters.push(Arc::new(m));
    }
    let last = Arc::make_mut(p.masters.last_mut().unwrap());
    last.layouts[5].name = "Zed".into();
    let zed = last.layouts[5].id;
    Arc::make_mut(&mut p.slides[3]).layout = zed;
    let order = |p: &Presentation| p.masters[0].layouts.iter().map(|l| l.shapes.iter().map(|s| s.ph_type()).collect::<Vec<_>>()).collect::<Vec<_>>();
    let back = deckcraft_odp::import(&deckcraft_odp::export(&p).unwrap()).unwrap();
    let l = back.masters.iter().flat_map(|m| &m.layouts).find(|l| l.id == back.slides[3].layout).unwrap();
    assert_eq!(l.name, "Zed");
    assert_eq!(order(&back), order(&p));
}

/// Review series 2, round 5: a two-colour gradient is one rule (two stops, at 0 and 1): an sRGB
/// one with a stop off the end is saved to `.pptx` as it is (PowerPoint draws it straight
/// anyway) and comes back as itself; and a page layout past the budget becomes its kind's
/// default (not the master's first layout).
#[test]
fn two_colour_gradients_and_layouts_past_the_budget() {
    use deckcraft_model::style::{Gradient, GradientBlend, GradientStop};
    let stop = |pos: f64, c: Rgba| GradientStop { pos, color: ColorRef::rgb(c) };
    let g = Gradient {
        stops: vec![stop(0.2, Rgba::rgb(255, 0, 0)), stop(1.0, Rgba::rgb(0, 0, 255))],
        shape: GradientShape::Linear { angle: 0.0, scaled: false },
        flip: None,
        rotate_with_shape: true,
        tile: [0.0; 4],
        blend: GradientBlend::Srgb,
    };
    let shape =
        Shape { xfrm: Some(Xfrm::new(50.0, 50.0, 200.0, 100.0)), geom: Geom::preset("rect"), fill: Some(Fill::Gradient(g)), ..Default::default() };
    let back = deckcraft_pptx::import(&deckcraft_pptx::export(&deck_with(vec![shape])).unwrap()).unwrap();
    let Some(Fill::Gradient(g)) = &shapes_of(&back)[0].fill else { panic!() };
    assert_eq!(g.stops.iter().map(|s| s.pos).collect::<Vec<_>>(), [0.2, 1.0]);

    // 1024 page layouts of ours with 64 placeholders each, on one master: the next, a title
    // slide's, is past the budget of placeholders.
    let boxes = r#"<presentation:placeholder presentation:object="outline" svg:x="1cm" svg:y="1cm" svg:width="1cm" svg:height="1cm"/>"#.repeat(64);
    let layouts: String = (0..1024)
        .map(|i| {
            format!(r#"<style:presentation-page-layout style:name="obj-{i}.M1" style:display-name="L{i}">{boxes}</style:presentation-page-layout>"#)
        })
        .chain(std::iter::once(r#"<style:presentation-page-layout style:name="title.M1" style:display-name="Cover"/>"#.to_string()))
        .collect();
    let doc = format!(
        r#"<office:document {NS} xmlns:presentation="urn:oasis:names:tc:opendocument:xmlns:presentation:1.0" office:mimetype="application/vnd.oasis.opendocument.presentation"><office:styles>{layouts}</office:styles><office:master-styles><style:master-page style:name="M1"/></office:master-styles><office:body><office:presentation><draw:page draw:master-page-name="M1" presentation:presentation-page-layout-name="title.M1"/></office:presentation></office:body></office:document>"#
    );
    let p = deckcraft_odp::import(doc.as_bytes()).unwrap();
    let l = p.masters[0].layouts.iter().find(|l| l.id == p.slides[0].layout);
    assert_eq!(l.map(|l| l.kind), Some(deckcraft_model::LayoutType::Title));
}

/// Review series 2, round 5 (correctness): a master our file names no layouts for has none (it got
/// the eleven defaults back), a page keeps its own master when its layout names another, saving
/// thousands of layouts takes no time to name them, and a ramp with a border keeps its stops.
#[test]
fn layouts_masters_and_ramps_as_written() {
    use deckcraft_model::style::GradientBlend;
    // Every layout with a look of its own: each reads back on a master of its own (a known
    // loss), and no defaults come back.
    let mut p = sample();
    let m = Arc::make_mut(&mut p.masters[0]);
    for l in &mut m.layouts {
        l.background = Some(deckcraft_model::Background::Fill { fill: Fill::solid(ColorRef::rgb(Rgba::rgb(9, 9, 9))) });
    }
    let count = |p: &Presentation| p.masters.iter().map(|m| m.layouts.len()).sum::<usize>();
    let back = deckcraft_odp::import(&deckcraft_odp::export(&p).unwrap()).unwrap();
    assert_eq!(count(&back), count(&p));
    // A page on another master than its layout names keeps its master.
    let mut p = sample();
    let mut second = (*p.masters[0]).clone();
    second.id = deckcraft_model::MasterId(70_001);
    second.name = "Second".into();
    for (j, l) in second.layouts.iter_mut().enumerate() {
        l.id = deckcraft_model::LayoutId(70_100 + j as u32);
    }
    p.masters.push(Arc::new(second));
    let bytes = deckcraft_odp::export(&p).unwrap();
    let content = part(&bytes, "content.xml").replacen(r#"draw:master-page-name="DeckCraft""#, r#"draw:master-page-name="Second""#, 1);
    let mut z = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let mut src = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
    for i in 0..src.len() {
        let mut f = src.by_index(i).unwrap();
        let name = f.name().to_string();
        let mut data = vec![];
        std::io::Read::read_to_end(&mut f, &mut data).unwrap();
        z.start_file(name.as_str(), zip::write::SimpleFileOptions::default()).unwrap();
        z.write_all(if name == "content.xml" { content.as_bytes() } else { &data }).unwrap();
    }
    let back = deckcraft_odp::import(&z.finish().unwrap().into_inner()).unwrap();
    let on = back.masters.iter().position(|m| m.layouts.iter().any(|l| l.id == back.slides[0].layout));
    assert_eq!(on.map(|i| back.masters[i].name.as_str()), Some("Second"));
    // Thousands of layouts of one kind are named at once.
    let mut p = sample();
    let m = Arc::make_mut(&mut p.masters[0]);
    let only = m.layouts[5].clone();
    for k in 0..3000 {
        let mut l = only.clone();
        l.id = deckcraft_model::LayoutId(200_000 + k);
        m.layouts.push(l);
    }
    let started = std::time::Instant::now();
    deckcraft_odp::export(&p).unwrap();
    assert!(started.elapsed().as_secs() < 10, "{:?}", started.elapsed());
    // Our ramp of PowerPoint's curve, with a border: not ours as written, so its stops are kept.
    let ramp = deckcraft_render::gradient_ramp(&[(0.0, Rgba::BLACK), (1.0, Rgba::WHITE)], GradientBlend::PowerPoint);
    let stops: String = ramp
        .iter()
        .map(|(p, c)| {
            format!(r##"<loext:gradient-stop svg:offset="{p}" loext:color-type="rgb" loext:color-value="#{:02x}{:02x}{:02x}"/>"##, c.r, c.g, c.b)
        })
        .collect();
    let doc = format!(
        r##"<office:document {NS} xmlns:loext="urn:org:documentfoundation:names:experimental:office:xmlns:loext:1.0" office:mimetype="application/vnd.oasis.opendocument.presentation"><office:styles><draw:gradient draw:name="G" draw:style="linear" draw:start-color="#000000" draw:end-color="#ffffff" draw:border="50%">{stops}</draw:gradient></office:styles><office:automatic-styles><style:style style:name="gr1" style:family="graphic"><style:graphic-properties draw:fill="gradient" draw:fill-gradient-name="G"/></style:style></office:automatic-styles><office:body><office:presentation><draw:page><draw:custom-shape draw:style-name="gr1" svg:x="1cm" svg:y="1cm" svg:width="4cm" svg:height="2cm"><draw:enhanced-geometry svg:viewBox="0 0 21600 21600" draw:type="rectangle"/></draw:custom-shape></draw:page></office:presentation></office:body></office:document>"##
    );
    let p = deckcraft_odp::import(doc.as_bytes()).unwrap();
    let Some(Fill::Gradient(g)) = &shapes_of(&p)[0].fill else { panic!() };
    assert!(g.stops.len() > ramp.len() && g.blend == GradientBlend::Srgb, "{} stops, {:?}", g.stops.len(), g.blend);
}

/// Final review (layouts): masters keep their order and names, a layout's colour map gives it a
/// look of its own, a layout's footers are its own (removed ones stay removed, moved ones moved),
/// and content and media placeholders on a layout stay placeholders of their kind.
#[test]
fn layouts_keep_order_footers_colour_maps_and_placeholder_kinds() {
    use deckcraft_model::{LayoutType, Placeholder};
    let mut p = sample();
    let mut evening = (*p.masters[0]).clone();
    evening.id = deckcraft_model::MasterId(60_001);
    for (j, l) in evening.layouts.iter_mut().enumerate() {
        l.id = deckcraft_model::LayoutId(60_100 + j as u32);
    }
    p.masters.push(Arc::new(evening));
    // The first slide on the second master (of the same name).
    Arc::make_mut(&mut p.slides[0]).layout = deckcraft_model::LayoutId(60_100);
    let m = Arc::make_mut(&mut p.masters[0]);
    // A dark section layout, by its colour map.
    let sec = m.layouts.iter_mut().find(|l| l.kind == LayoutType::SectionHeader).unwrap();
    sec.color_map = Some(vec![("bg1".into(), "dk1".into()), ("tx1".into(), "lt1".into())]);
    let sec_id = sec.id;
    // Title Only without footers; Blank with a moved footer, a content and a media placeholder.
    let only = m.layouts.iter_mut().find(|l| l.kind == LayoutType::TitleOnly).unwrap();
    only.shapes.retain(|s| !s.ph_type().is_some_and(PhType::is_footer_kind));
    let blank = m.layouts.iter_mut().find(|l| l.kind == LayoutType::Blank).unwrap();
    for s in blank.shapes.iter_mut().filter(|s| s.ph_type() == Some(PhType::Footer)) {
        s.xfrm = Some(Xfrm::new(10.0, 10.0, 100.0, 20.0));
    }
    let ph = |kind: PhType, x: f64| Shape {
        xfrm: Some(Xfrm::new(x, 100.0, 200.0, 100.0)),
        ph: Some(Placeholder { kind, idx: 5, ..Default::default() }),
        ..Default::default()
    };
    blank.shapes.extend([ph(PhType::Obj, 50.0), ph(PhType::Media, 300.0)]);
    Arc::make_mut(&mut p.slides[2]).layout = sec_id;
    let back = deckcraft_odp::import(&deckcraft_odp::export(&p).unwrap()).unwrap();
    let names: Vec<&str> = back.masters.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(&names[..2], ["DeckCraft", "DeckCraft"], "{names:?}");
    let kinds = |p: &Presentation, kind: LayoutType| {
        p.masters[0].layouts.iter().find(|l| l.kind == kind).map(|l| l.shapes.iter().filter_map(|s| s.ph_type()).collect::<Vec<_>>())
    };
    assert_eq!(kinds(&back, LayoutType::TitleOnly), kinds(&p, LayoutType::TitleOnly));
    let blank_back = back.masters[0].layouts.iter().find(|l| l.kind == LayoutType::Blank).unwrap();
    let footer = blank_back.shapes.iter().find(|s| s.ph_type() == Some(PhType::Footer)).and_then(|s| s.xfrm).unwrap();
    assert_eq!((footer.x.round(), footer.y.round()), (10.0, 10.0));
    let phs: Vec<PhType> = blank_back.shapes.iter().filter_map(|s| s.ph_type()).filter(|k| !k.is_footer_kind()).collect();
    assert_eq!(phs.len(), 2, "{phs:?}");
    assert!(phs.contains(&PhType::Obj), "{phs:?}");
    // The section slide keeps its dark look: it's drawn on a page of its own, whose background
    // is the colour map's (ODF has no colour maps, so the look is what's kept).
    let slide_master = back.masters.iter().find(|m| m.layouts.iter().any(|l| l.id == back.slides[2].layout)).unwrap();
    let bg = slide_master.background.as_ref().and_then(|b| match b {
        deckcraft_model::Background::Fill { fill: Fill::Solid { color } } => Some(color.resolve(&slide_master.theme.colors, None)),
        _ => None,
    });
    assert!(bg.is_some_and(|c| u32::from(c.r) + u32::from(c.g) + u32::from(c.b) < 200), "{bg:?} on {}", slide_master.name);
}

/// Final review (LibreOffice): a frame's vertical text direction is in its paragraph properties
/// (LibreOffice reads it only there); bar spacing is on the value axis too; a pie has a reversed
/// value axis (LibreOffice draws it clockwise then); slices are parted in white, bars without an
/// outline get none; the legend has no border.
#[test]
fn charts_and_vertical_text_as_libreoffice_reads_them() {
    use deckcraft_model::chart::{Chart, ChartType};
    use deckcraft_model::text::TextDir;
    let mut tb =
        Shape { xfrm: Some(Xfrm::new(20.0, 20.0, 100.0, 300.0)), text_box: true, text: Some(TextBody::from_text("down")), ..Default::default() };
    tb.text.as_mut().unwrap().body.vert = Some(TextDir::EaVertical);
    let chart = |kind: ChartType, y: f64| {
        let mut c = Chart::sample(kind);
        c.legend = Some("r".into());
        Shape { xfrm: Some(Xfrm::new(200.0, y, 300.0, 150.0)), kind: ShapeKind::Chart(Box::new(c)), ..Default::default() }
    };
    let p = deck_with(vec![tb, chart(ChartType::Column, 20.0), chart(ChartType::Pie, 200.0)]);
    let bytes = deckcraft_odp::export(&p).unwrap();
    let content = part(&bytes, "content.xml");
    assert!(content.contains(r#"<style:paragraph-properties style:writing-mode="tb-rl"/>"#), "{content}");
    let column = part(&bytes, "Object 1/content.xml");
    let y_axis = column.split("chart:name=\"primary-y\" chart:style-name=\"").nth(1).and_then(|s| s.split('"').next()).unwrap();
    let y_style = column.split(&format!("style:name=\"{y_axis}\"")).nth(1).unwrap();
    assert!(y_style.split("</style:style>").next().unwrap().contains("chart:gap-width"), "{column}");
    assert!(column.contains(r#"<chart:legend chart:legend-position="end" style:legend-expansion="balanced" chart:style-name="#));
    let pie = part(&bytes, "Object 2/content.xml");
    assert!(pie.contains(r#"chart:reverse-direction="true""#) && pie.contains(r##"svg:stroke-color="#ffffff""##), "{pie}");
    let back = deckcraft_odp::import(&bytes).unwrap();
    let s = shapes_of(&back);
    assert_eq!(s[0].text.as_ref().unwrap().body.vert, Some(TextDir::EaVertical));
    let ShapeKind::Chart(c) = &s[1].kind else { panic!() };
    let ShapeKind::Chart(orig) = &shapes_of(&p)[1].kind else { panic!() };
    assert!((c.gap_width - orig.gap_width).abs() < 0.01 && (c.overlap - orig.overlap).abs() < 0.01);
}

/// Final fuzzing: document budgets for what every shape or frame holds a copy of: paragraphs
/// (each costs kilobytes, however short), gradient stops, and chart points (a chart shown by many
/// frames); and other apps' page layouts are read without copying the whole master each time.
#[test]
fn per_copy_costs_have_document_budgets() {
    let ns = format!(
        r#"{NS} xmlns:presentation="urn:oasis:names:tc:opendocument:xmlns:presentation:1.0" xmlns:loext="urn:org:documentfoundation:names:experimental:office:xmlns:loext:1.0" xmlns:xlink="http://www.w3.org/1999/xlink""#
    );
    let doc = |styles: &str, body: &str| {
        format!(
            r#"<office:document {ns} office:mimetype="application/vnd.oasis.opendocument.presentation"><office:styles>{styles}</office:styles><office:master-styles><style:master-page style:name="M"/></office:master-styles><office:body><office:presentation>{body}</office:presentation></office:body></office:document>"#
        )
    };
    let frame = |inner: &str| format!(r#"<draw:frame svg:x="1cm" svg:y="1cm" svg:width="5cm" svg:height="5cm">{inner}</draw:frame>"#);
    // Paragraphs: six bodies of 20 000.
    let body = frame(&format!("<draw:text-box>{}</draw:text-box>", "<text:p>x</text:p>".repeat(20_000))).repeat(6);
    let p = deckcraft_odp::import(doc("", &format!("<draw:page>{body}</draw:page>")).as_bytes()).unwrap();
    let paragraphs: usize = p.slides[0].shapes.iter().filter_map(|s| s.text.as_ref()).map(|t| t.paragraphs.len()).sum();
    assert!(paragraphs <= 100_000, "{paragraphs}");
    // Gradient stops: a 256-stop axial gradient (514 stops laid out) on 5000 shapes.
    let stops: String = (0..256)
        .map(|i| {
            format!(
                r##"<loext:gradient-stop svg:offset="{}" loext:color-type="rgb" loext:color-value="#{:06x}"/>"##,
                f64::from(i) / 255.0,
                (i * 7919) & 0xff_ffff
            )
        })
        .collect();
    let styles = format!(
        r#"<draw:gradient draw:name="g" draw:style="axial" draw:border="10%">{stops}</draw:gradient><style:style style:name="gr" style:family="graphic"><style:graphic-properties draw:fill="gradient" draw:fill-gradient-name="g"/></style:style>"#
    );
    let rects = r#"<draw:rect draw:style-name="gr" svg:x="1cm" svg:y="1cm" svg:width="1cm" svg:height="1cm"/>"#.repeat(5000);
    let p = deckcraft_odp::import(doc(&styles, &format!("<draw:page>{rects}</draw:page>")).as_bytes()).unwrap();
    let total: usize = p.slides[0].shapes.iter().filter_map(|s| if let Some(Fill::Gradient(g)) = &s.fill { Some(g.stops.len()) } else { None }).sum();
    assert!(total <= (1 << 20) + 2 * 5000, "{total}");
    // Chart points: one chart of 256 series × 4096 points, shown by 20 frames.
    let series = r#"<chart:series chart:values-cell-range-address="local-table.$B$2:.$B$4097"><chart:domain table:cell-range-address="local-table.$C$2:.$C$4097"/><chart:domain table:cell-range-address="local-table.$D$2:.$D$4097"/></chart:series>"#.repeat(256);
    let chart = format!(
        r#"<office:document-content {ns}><office:body><office:chart><chart:chart chart:class="chart:bubble"><chart:plot-area>{series}</chart:plot-area><table:table table:name="local-table"><table:table-rows><table:table-row table:number-rows-repeated="5000"><table:table-cell office:value-type="float" office:value="1" table:number-columns-repeated="13"/></table:table-row></table:table-rows></table:table></chart:chart></office:chart></office:body></office:document-content>"#
    );
    let content = format!(
        r#"<office:document-content {ns}><office:body><office:presentation><draw:page>{}</draw:page></office:presentation></office:body></office:document-content>"#,
        frame(r#"<draw:object xlink:href="./Object 1"/>"#).repeat(20)
    );
    let mut z = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (name, data) in
        [("mimetype", "application/vnd.oasis.opendocument.presentation"), ("content.xml", content.as_str()), ("Object 1/content.xml", chart.as_str())]
    {
        z.start_file(name, zip::write::SimpleFileOptions::default()).unwrap();
        z.write_all(data.as_bytes()).unwrap();
    }
    let p = deckcraft_odp::import(&z.finish().unwrap().into_inner()).unwrap();
    let mut points = 0usize;
    deckcraft_model::walk(&p.slides[0].shapes, &mut |s, _| {
        if let ShapeKind::Chart(c) = &s.kind {
            points += c.series.iter().map(|s| s.values.len() + s.x_values.len() + s.sizes.len()).sum::<usize>();
        }
    });
    assert!(points <= 1 << 20, "{points}");
    // Other apps' page layouts: a thousand described ones, a page each.
    let layouts: String = (0..1000).map(|i| format!(r#"<style:presentation-page-layout style:name="L{i}" style:display-name="Lay {i}"/>"#)).collect();
    let pages: String =
        (0..1000).map(|i| format!(r#"<draw:page draw:master-page-name="M" presentation:presentation-page-layout-name="L{i}"/>"#)).collect();
    let started = std::time::Instant::now();
    let p = deckcraft_odp::import(doc(&layouts, &pages).as_bytes()).unwrap();
    assert_eq!(p.slides.len(), 1000);
    assert!(started.elapsed().as_secs() < 10, "{:?}", started.elapsed());
}

/// Moving files between apps: a line LibreOffice draws solid (`draw:stroke="dash"` with no dash
/// defined, as it writes one) stays solid; it was read as a dashed line and saved dashed.
#[test]
fn a_dash_without_a_definition_is_solid() {
    let doc = format!(
        r#"<office:document {NS} office:mimetype="application/vnd.oasis.opendocument.presentation"><office:automatic-styles><style:style style:name="gr1" style:family="graphic"><style:graphic-properties draw:stroke="dash" svg:stroke-width="0.05cm" svg:stroke-color="000000"/></style:style><style:style style:name="gr2" style:family="graphic"><style:graphic-properties draw:stroke="dash" draw:stroke-dash="Missing" svg:stroke-width="0.05cm"/></style:style></office:automatic-styles><office:body><office:presentation><draw:page><draw:line draw:style-name="gr1" svg:x1="1cm" svg:y1="1cm" svg:x2="10cm" svg:y2="1cm"/><draw:line draw:style-name="gr2" svg:x1="1cm" svg:y1="3cm" svg:x2="10cm" svg:y2="3cm"/></draw:page></office:presentation></office:body></office:document>"#
    );
    let p = deckcraft_odp::import(doc.as_bytes()).unwrap();
    for s in shapes_of(&p) {
        assert_eq!(s.line.as_ref().and_then(|l| l.dash.clone()), None, "{:?}", s.line);
    }
    let content = part(&deckcraft_odp::export(&p).unwrap(), "content.xml");
    assert!(!content.contains(r#"draw:stroke="dash""#), "{content}");
}

/// Moving files between apps: a gradient fill naming no gradient (LibreOffice writes one set
/// through its API so) is LibreOffice's default, black to white; it was read as the default
/// solid colour, so a master's gradient background came back flat blue.
#[test]
fn a_gradient_without_a_definition_is_black_to_white() {
    let doc = format!(
        r#"<office:document {NS} xmlns:presentation="urn:oasis:names:tc:opendocument:xmlns:presentation:1.0" office:mimetype="application/vnd.oasis.opendocument.presentation"><office:automatic-styles><style:style style:name="gr1" style:family="graphic"><style:graphic-properties draw:fill="gradient"/></style:style></office:automatic-styles><office:body><office:presentation><draw:page><draw:custom-shape draw:style-name="gr1" svg:x="1cm" svg:y="1cm" svg:width="4cm" svg:height="2cm"><draw:enhanced-geometry svg:viewBox="0 0 21600 21600" draw:type="rectangle"/></draw:custom-shape></draw:page></office:presentation></office:body></office:document>"#
    );
    let p = deckcraft_odp::import(doc.as_bytes()).unwrap();
    let Some(Fill::Gradient(g)) = &shapes_of(&p)[0].fill else { panic!("{:?}", shapes_of(&p)[0].fill) };
    let rgb = |s: &deckcraft_model::style::GradientStop| {
        s.color.resolve(&deckcraft_color::ColorScheme { name: String::new(), colors: [Rgba::BLACK; 12] }, None)
    };
    assert_eq!((rgb(g.stops.first().unwrap()), rgb(g.stops.last().unwrap())), (Rgba::BLACK, Rgba::WHITE));
}
