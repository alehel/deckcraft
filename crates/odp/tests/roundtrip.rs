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
        let shape = Shape { xfrm: Some(Xfrm::new(50.0, 50.0, 400.0, 300.0)), kind: ShapeKind::Chart(Box::new(chart)), ..Default::default() };
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
