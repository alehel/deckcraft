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
    assert_eq!((t.n_rows(), t.n_cols()), (2, 3));
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
    let starts_written = |paras: Vec<Paragraph>| {
        let shape = Shape {
            xfrm: Some(Xfrm::new(50.0, 50.0, 300.0, 200.0)),
            text_box: true,
            text: Some(TextBody { paragraphs: paras, ..Default::default() }),
            ..Default::default()
        };
        let xml = part(&deckcraft_odp::export(&deck_with(vec![shape])).unwrap(), "content.xml");
        let page = &xml[xml.find("<draw:page ").unwrap()..];
        page.match_indices("text:start-value=\"").map(|(i, m)| page[i + m.len()..].split('"').next().unwrap().to_string()).collect::<Vec<_>>()
    };
    assert_eq!(starts_written(vec![para("a", Some(3)), para("b", Some(3)), para("c", Some(3))]), ["3"]);
    let mixed = vec![para("a", Some(1)), para("b", Some(1)), para("plain", None), para("c", Some(1)), para("d", Some(7)), para("e", Some(7))];
    assert_eq!(starts_written(mixed), ["1", "1", "7"]);
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
        kind: ShapeKind::Media(deckcraft_model::MediaClip { media: clip, video: true, loop_play: true, volume: 1.0, ..Default::default() }),
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
    assert!(m.video && m.loop_play);
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
        kind: ShapeKind::Connector { start: Some((ids[0], 3)), end: Some((ids[1], 1)) },
        geom: Geom::preset("straightConnector1"),
        line: Some(deckcraft_model::style::Line::solid(ColorRef::rgb(Rgba::BLACK), 1.5)),
        ..Default::default()
    };
    Arc::make_mut(&mut p.slides[0]).shapes.push(c);
    let back = deckcraft_odp::import(&deckcraft_odp::export(&p).unwrap()).unwrap();
    let s = shapes_of(&back);
    let ShapeKind::Connector { start, end } = &s[2].kind else { panic!("{:?}", s[2].kind) };
    assert_eq!(*start, Some((s[0].id, 3)));
    assert_eq!(*end, Some((s[1].id, 1)));
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
