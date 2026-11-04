//! Slide transitions and object animations in a page's timing tree (`anim:par` under
//! `presentation:node-type="timing-root"`), read into the model and written back as the SMIL
//! nodes LibreOffice plays.

use std::collections::HashMap;
use std::fmt::Write as _;

use deckcraft_model::anim::{AnimClass, AnimStart, Animation};
use deckcraft_model::{ShapeId, Transition};

use crate::dom::{El, attr, esc, kids};

/// Most effects read from one page.
const MAX_EFFECTS: usize = 2_000;

/// Our effect id and option for a LibreOffice preset id and sub-type.
fn effect_of(class: AnimClass, preset: &str, sub: &str) -> (String, String) {
    let name = preset
        .trim_start_matches("ooo-")
        .trim_start_matches("entrance-")
        .trim_start_matches("exit-")
        .trim_start_matches("emphasis-")
        .trim_start_matches("motionpath-");
    let dir = |s: &str| {
        match s {
            "from-bottom" | "from-top-to-bottom" => "b",
            "from-top" => "t",
            "from-left" => "l",
            "from-right" => "r",
            "from-bottom-left" => "lb",
            "from-top-left" => "lt",
            "from-top-right" => "rt",
            "from-bottom-right" => "rb",
            "horizontal" => "horz",
            "vertical" => "vert",
            "in" => "in",
            "out" => "out",
            "across" => "across",
            "downward" => "down",
            "horizontal-in" => "horzIn",
            "horizontal-out" => "horzOut",
            "vertical-in" => "vertIn",
            "vertical-out" => "vertOut",
            "1" | "2" | "3" | "4" | "8" => s,
            _ => "",
        }
        .to_string()
    };
    let e = match (class, name) {
        (AnimClass::Entrance, "appear") => "appear",
        (AnimClass::Entrance, "fade-in" | "fade-in-and-swivel") => "fade",
        (AnimClass::Entrance, "fly-in" | "fly-in-slow") => "fly",
        (AnimClass::Entrance, "venetian-blinds") => "blinds",
        (AnimClass::Entrance, "box") => "box",
        (AnimClass::Entrance, "checkerboard") => "checkerboard",
        (AnimClass::Entrance, "circle") => "circle",
        (AnimClass::Entrance, "diamond") => "diamond",
        (AnimClass::Entrance, "dissolve-in") => "dissolve",
        (AnimClass::Entrance, "peek-in") => "peek",
        (AnimClass::Entrance, "random-bars") => "randomBars",
        (AnimClass::Entrance, "split") => "split",
        (AnimClass::Entrance, "wheel") => "wheel",
        (AnimClass::Entrance, "wipe") => "wipe",
        (AnimClass::Entrance, "zoom" | "fade-in-and-zoom") => "zoom",
        (AnimClass::Entrance, "expand") => "expand",
        (AnimClass::Entrance, "ascend" | "rise-up") => "rise",
        (AnimClass::Entrance, "spinner" | "spin-in") => "spinner",
        (AnimClass::Entrance, "swivel") => "swivel",
        (AnimClass::Entrance, "float" | "float-in") => "float",
        (AnimClass::Entrance, "grow-and-turn") => "grow",
        (AnimClass::Entrance, "boomerang" | "bounce") => "bounce",
        (AnimClass::Entrance, "diagonal-squares" | "strips") => "strips",
        (AnimClass::Entrance, _) => "fade",
        (AnimClass::Exit, "disappear") => "disappear",
        (AnimClass::Exit, "fade-out") => "fadeOut",
        (AnimClass::Exit, "fly-out" | "fly-out-slow") => "flyOut",
        (AnimClass::Exit, "venetian-blinds") => "blindsOut",
        (AnimClass::Exit, "box") => "boxOut",
        (AnimClass::Exit, "checkerboard") => "checkerboardOut",
        (AnimClass::Exit, "circle") => "shapeOut",
        (AnimClass::Exit, "diamond") => "diamondOut",
        (AnimClass::Exit, "dissolve") => "dissolveOut",
        (AnimClass::Exit, "peek-out") => "peekOut",
        (AnimClass::Exit, "random-bars") => "randomBarsOut",
        (AnimClass::Exit, "split") => "splitOut",
        (AnimClass::Exit, "wheel") => "wheelOut",
        (AnimClass::Exit, "wipe") => "wipeOut",
        (AnimClass::Exit, "zoom" | "fade-out-and-zoom") => "zoomOut",
        (AnimClass::Exit, "contract") => "contract",
        (AnimClass::Exit, "descend" | "sink-down") => "sinkDown",
        (AnimClass::Exit, "spinner" | "spin-out") => "spinnerOut",
        (AnimClass::Exit, "swivel") => "swivelOut",
        (AnimClass::Exit, "diagonal-squares" | "strips") => "stripsOut",
        (AnimClass::Exit, _) => "fadeOut",
        (AnimClass::Emphasis, "spin") => "spin",
        (AnimClass::Emphasis, "grow-and-shrink") => "growShrink",
        (AnimClass::Emphasis, "transparency") => "transparency",
        (AnimClass::Emphasis, "fill-color") => "fillColor",
        (AnimClass::Emphasis, "font-color") => "fontColor",
        (AnimClass::Emphasis, "line-color") => "lineColor",
        (AnimClass::Emphasis, "bold-flash") => "boldFlash",
        (AnimClass::Emphasis, "teeter") => "teeter",
        (AnimClass::Emphasis, "wave") => "wave",
        (AnimClass::Emphasis, "lighten") => "lighten",
        (AnimClass::Emphasis, "darken") => "darken",
        (AnimClass::Emphasis, "desaturate") => "desaturate",
        (AnimClass::Emphasis, "complementary-color") => "complementaryColor",
        (AnimClass::Emphasis, "color-blend" | "color-over-by-word" | "color-over-by-letter") => "fontColor",
        (AnimClass::Emphasis, "underline") => "underline",
        (AnimClass::Emphasis, _) => "pulse",
        (AnimClass::Path, _) => "customPath",
        (AnimClass::Media, _) => "play",
    };
    let opt = match e {
        "spin" => if sub.contains("counter") { "counterClockwise" } else { "clockwise" }.to_string(),
        _ => dir(sub),
    };
    (e.to_string(), opt)
}

/// The LibreOffice preset id and sub-type for our effect.
fn preset_of(a: &Animation) -> (String, String) {
    let class = match a.class {
        AnimClass::Entrance => "entrance",
        AnimClass::Exit => "exit",
        AnimClass::Emphasis => "emphasis",
        AnimClass::Path => "motionpath",
        AnimClass::Media => "media",
    };
    let name = match a.effect.as_str() {
        "appear" => "appear",
        "fade" => "fade-in",
        "fly" => "fly-in",
        "blinds" | "blindsOut" => "venetian-blinds",
        "box" | "boxOut" => "box",
        "checkerboard" | "checkerboardOut" => "checkerboard",
        "circle" | "shape" | "shapeOut" => "circle",
        "diamond" | "diamondOut" => "diamond",
        "dissolve" => "dissolve-in",
        "dissolveOut" => "dissolve",
        "peek" => "peek-in",
        "peekOut" => "peek-out",
        "randomBars" | "randomBarsOut" => "random-bars",
        "split" | "splitOut" => "split",
        "wheel" | "wheelOut" => "wheel",
        "wipe" | "wipeOut" => "wipe",
        "zoom" => "fade-in-and-zoom",
        "zoomOut" => "fade-out-and-zoom",
        "expand" => "expand",
        "contract" => "contract",
        "rise" => "ascend",
        "sinkDown" => "descend",
        "spinner" | "spinnerOut" => "spinner",
        "swivel" | "swivelOut" => "swivel",
        "float" | "floatOut" => "float",
        "grow" => "grow-and-turn",
        "bounce" | "bounceOut" => "boomerang",
        "strips" | "stripsOut" => "diagonal-squares",
        "disappear" => "disappear",
        "fadeOut" => "fade-out",
        "flyOut" => "fly-out",
        "spin" => "spin",
        "growShrink" => "grow-and-shrink",
        "transparency" => "transparency",
        "fillColor" => "fill-color",
        "fontColor" => "font-color",
        "lineColor" => "line-color",
        "boldFlash" => "bold-flash",
        "teeter" => "teeter",
        "wave" => "wave",
        "lighten" => "lighten",
        "darken" => "darken",
        "desaturate" => "desaturate",
        "complementaryColor" => "complementary-color",
        "underline" => "underline",
        "pulse" | "colorPulse" => "pulse",
        _ if a.class == AnimClass::Path => "custom",
        _ => "fade-in",
    };
    let sub = match a.option.as_str() {
        "b" => "from-bottom",
        "t" => "from-top",
        "l" => "from-left",
        "r" => "from-right",
        "lb" => "from-bottom-left",
        "lt" => "from-top-left",
        "rt" => "from-top-right",
        "rb" => "from-bottom-right",
        "horz" => "horizontal",
        "vert" => "vertical",
        "in" => "in",
        "out" => "out",
        "across" => "across",
        "down" => "downward",
        "horzIn" => "horizontal-in",
        "horzOut" => "horizontal-out",
        "vertIn" => "vertical-in",
        "vertOut" => "vertical-out",
        "clockwise" => "",
        o => o,
    };
    (format!("ooo-{class}-{name}"), sub.to_string())
}

/// A SMIL clock value in ms (`1.5s`, `500ms`, `0.001s`, bare seconds).
fn clock(s: &str) -> Option<u32> {
    let s = s.trim();
    let v = if let Some(ms) = s.strip_suffix("ms") {
        ms.trim().parse::<f64>().ok()?
    } else if let Some(sec) = s.strip_suffix('s') {
        sec.trim().parse::<f64>().ok()? * 1000.0
    } else {
        s.parse::<f64>().ok()? * 1000.0
    };
    (v.is_finite() && v >= 0.0).then(|| v.min(f64::from(u32::MAX)).round() as u32)
}

/// Where an `smil:targetElement` points: a shape, or one of its paragraphs.
pub type Targets = HashMap<String, (ShapeId, Option<u32>)>;

/// What a page's timing root holds.
#[derive(Default)]
pub struct Timing {
    pub transition: Option<Transition>,
    pub animations: Vec<Animation>,
}

/// Read the timing root of a page.
pub fn read(root: &El, page_id: Option<&str>, targets: &Targets) -> Timing {
    let mut t = Timing::default();
    for e in root.elements() {
        match (e.name.as_str(), attr(e, "presentation:node-type")) {
            ("anim:seq", Some("main-sequence")) => read_sequence(e, None, targets, &mut t.animations),
            ("anim:seq", Some("interactive-sequence")) => {
                // Starts on a click on its trigger shape (`idX.click`).
                let trigger = attr(e, "smil:begin").and_then(|b| b.strip_suffix(".click")).and_then(|id| targets.get(id)).map(|(s, _)| *s);
                read_sequence(e, trigger, targets, &mut t.animations);
            }
            ("anim:par", _) => {
                // The slide transition: a par that begins with the page.
                let begins_with_page = attr(e, "smil:begin").is_some_and(|b| b.ends_with(".begin") && page_id.is_none_or(|p| b.starts_with(p)));
                if let Some(f) = kids(e, "anim:transitionFilter").next().filter(|_| begins_with_page) {
                    t.transition = Some(transition(f));
                }
            }
            _ => {}
        }
    }
    t
}

fn transition(f: &El) -> Transition {
    let ty = attr(f, "smil:type").unwrap_or("fade");
    let sub = attr(f, "smil:subtype").unwrap_or("");
    let reverse = attr(f, "smil:direction") == Some("reverse");
    let side = |s: &str| match (s, reverse) {
        ("fromLeft" | "leftToRight", false) | ("fromRight" | "rightToLeft", true) => "r",
        ("fromRight" | "rightToLeft", false) | ("fromLeft" | "leftToRight", true) => "l",
        ("fromTop" | "topToBottom", false) | ("fromBottom" | "bottomToTop", true) => "d",
        _ => "u",
    };
    let (kind, option) = match ty {
        "fade" => ("fade", ""),
        "pushWipe" => ("push", side(sub)),
        // `fromLeft` enters from the left: it moves right.
        "slideWipe" => (if reverse { "uncover" } else { "cover" }, side(sub)),
        "barWipe" => ("wipe", side(sub)),
        "barnDoorWipe" => ("split", if sub == "horizontal" { "horzOut" } else { "vertOut" }),
        "irisWipe" | "ellipseWipe" => ("circle", ""),
        "clockWipe" | "pinWheelWipe" => ("wheel", ""),
        "blindsWipe" => ("blinds", if sub == "horizontal" { "horz" } else { "vert" }),
        "checkerBoardWipe" => ("checker", ""),
        "randomBarWipe" => ("randomBar", if sub == "horizontal" { "horz" } else { "vert" }),
        "dissolve" => ("dissolve", ""),
        "zoom" => ("zoom", ""),
        _ => ("fade", ""),
    };
    Transition {
        kind: kind.into(),
        option: option.into(),
        duration_ms: attr(f, "smil:dur").and_then(clock).unwrap_or(700).min(60_000),
        advance_on_click: true,
        advance_after_ms: None,
        sound: None,
        sound_loop: false,
        stop_sound: false,
        raw: None,
    }
}

fn read_sequence(seq: &El, trigger: Option<ShapeId>, targets: &Targets, out: &mut Vec<Animation>) {
    // Click groups → time groups → effects.
    for group in kids(seq, "anim:par") {
        for timed in kids(group, "anim:par") {
            let t0 = attr(timed, "smil:begin").and_then(clock).unwrap_or(0);
            for effect in kids(timed, "anim:par").chain(kids(timed, "anim:iterate")) {
                if out.len() >= MAX_EFFECTS {
                    return;
                }
                if let Some(a) = read_effect(effect, t0, trigger, targets) {
                    out.push(a);
                }
            }
        }
    }
}

/// The first `smil:targetElement` under a node, and the longest duration of its animations.
fn scan(el: &El, target: &mut Option<String>, dur: &mut u32, path: &mut Option<String>, depth: usize) {
    if depth > 16 {
        return;
    }
    for e in el.elements() {
        if target.is_none() {
            *target = attr(e, "smil:targetElement").map(str::to_string);
        }
        if e.name == "anim:animateMotion" && path.is_none() {
            *path = attr(e, "svg:path").map(str::to_string);
        }
        // The 1 ms visibility switch isn't the effect's length.
        if let Some(d) = attr(e, "smil:dur").and_then(clock).filter(|d| *d > 1) {
            *dur = (*dur).max(d);
        }
        scan(e, target, dur, path, depth + 1);
    }
}

fn read_effect(e: &El, t0: u32, trigger: Option<ShapeId>, targets: &Targets) -> Option<Animation> {
    let class = match attr(e, "presentation:preset-class").unwrap_or("custom") {
        "entrance" => AnimClass::Entrance,
        "exit" => AnimClass::Exit,
        "emphasis" => AnimClass::Emphasis,
        "motion-path" => AnimClass::Path,
        "media-call" => AnimClass::Media,
        _ => return None,
    };
    let mut target = attr(e, "smil:targetElement").map(str::to_string);
    let (mut dur, mut path) = (0u32, None);
    scan(e, &mut target, &mut dur, &mut path, 0);
    let (shape, paragraph) = *targets.get(target.as_deref()?)?;
    let (effect, option) = effect_of(class, attr(e, "presentation:preset-id").unwrap_or(""), attr(e, "presentation:preset-sub-type").unwrap_or(""));
    let start = match attr(e, "presentation:node-type") {
        Some("with-previous") => AnimStart::WithPrevious,
        Some("after-previous") => AnimStart::AfterPrevious,
        _ => AnimStart::OnClick,
    };
    // The delay is the effect's own: an after-previous group already starts when the previous ends.
    let _ = t0;
    let delay = attr(e, "smil:begin").and_then(clock).unwrap_or(0);
    let mut a = Animation {
        shape,
        class,
        effect,
        option,
        start,
        duration_ms: if dur == 0 && class != AnimClass::Entrance && class != AnimClass::Exit { 500 } else { dur }.min(600_000),
        delay_ms: delay.min(600_000),
        paragraph,
        trigger,
        ..Default::default()
    };
    if let Some(p) = path.filter(|_| class == AnimClass::Path) {
        a.path = Some(if p.trim_end().ends_with('E') { p } else { format!("{} E", p.trim_end()) });
    }
    if let Some(r) = e.elements().find_map(|c| attr(c, "smil:repeatCount")).and_then(|r| r.parse::<f64>().ok()).filter(|r| r.is_finite()) {
        a.repeat = (r.round() as u32).clamp(1, 1000);
    }
    a.auto_reverse = e.elements().any(|c| attr(c, "smil:autoReverse") == Some("true"));
    if let Some(acc) = e.elements().find_map(|c| attr(c, "smil:accelerate")).and_then(|v| v.parse::<f64>().ok()) {
        a.smooth_start = acc.clamp(0.0, 1.0);
    }
    if let Some(dec) = e.elements().find_map(|c| attr(c, "smil:decelerate")).and_then(|v| v.parse::<f64>().ok()) {
        a.smooth_end = dec.clamp(0.0, 1.0);
    }
    Some(a)
}

// ---------------------------------------------------------------------------------------------
// Writing

/// The SMIL nodes that make LibreOffice play one effect on `t`.
fn nodes(a: &Animation, t: &str) -> String {
    let dur = format!("{}s", f64::from(a.duration_ms.max(1)) / 1000.0);
    let show = format!(
        r#"<anim:set smil:begin="0s" smil:dur="0.001s" smil:fill="hold" smil:targetElement="{t}" smil:attributeName="visibility" smil:to="visible"/>"#
    );
    let hide = format!(
        r#"<anim:set smil:begin="{dur}" smil:dur="0.001s" smil:fill="hold" smil:targetElement="{t}" smil:attributeName="visibility" smil:to="hidden"/>"#
    );
    let filter = |ty: &str, sub: &str, out: bool| {
        let mode = if out { r#" smil:mode="out""# } else { "" };
        format!(r#"<anim:transitionFilter smil:dur="{dur}" smil:fill="hold" smil:targetElement="{t}" smil:type="{ty}" smil:subtype="{sub}"{mode}/>"#)
    };
    let dir_sub = |o: &str, horiz: &str, vert: &str| if matches!(o, "l" | "r" | "horz") { horiz.to_string() } else { vert.to_string() };
    let base = a.effect.trim_end_matches("Out");
    let wipe = match base {
        "fade" | "dissolve" => Some(("fade", "crossfade".to_string())),
        "wipe" => Some(("barWipe", dir_sub(&a.option, "leftToRight", "topToBottom"))),
        "blinds" => Some(("blindsWipe", if a.option == "vert" { "vertical".into() } else { "horizontal".into() })),
        "box" => Some(("irisWipe", "rectangle".into())),
        "checkerboard" => Some(("checkerBoardWipe", if a.option == "down" { "down".into() } else { "across".into() })),
        "circle" | "shape" => Some(("ellipseWipe", "circle".into())),
        "diamond" => Some(("irisWipe", "diamond".into())),
        "randomBars" => Some(("randomBarWipe", if a.option == "vert" { "vertical".into() } else { "horizontal".into() })),
        "split" => Some(("barnDoorWipe", if a.option.starts_with("vert") { "vertical".into() } else { "horizontal".into() })),
        "wheel" => Some((
            "pinWheelWipe",
            format!("{}BladeVertical", if a.option.is_empty() { "one".to_string() } else { a.option.clone() }).replace("1Blade", "oneBlade"),
        )),
        "strips" => Some(("waterfallWipe", "horizontalLeft".into())),
        "peek" | "rise" | "sinkDown" | "spinner" | "swivel" | "float" | "bounce" => Some(("fade", "crossfade".to_string())),
        _ => None,
    };
    let fly = |out: bool| {
        let (ax, vx, ay, vy) = match a.option.as_str() {
            "l" => ("x", "0-width/2", "y", "y"),
            "r" => ("x", "1+width/2", "y", "y"),
            "t" => ("x", "x", "y", "0-height/2"),
            "lt" => ("x", "0-width/2", "y", "0-height/2"),
            "rt" => ("x", "1+width/2", "y", "0-height/2"),
            "lb" => ("x", "0-width/2", "y", "1+height/2"),
            "rb" => ("x", "1+width/2", "y", "1+height/2"),
            _ => ("x", "x", "y", "1+height/2"),
        };
        let (fx, tx, fy, ty) = if out {
            (ax.to_string(), vx.to_string(), ay.to_string(), vy.to_string())
        } else {
            (vx.to_string(), ax.to_string(), vy.to_string(), ay.to_string())
        };
        format!(
            r#"<anim:animate smil:dur="{dur}" smil:fill="hold" smil:targetElement="{t}" smil:attributeName="x" smil:values="{fx};{tx}" smil:keyTimes="0;1" presentation:additive="base"/><anim:animate smil:dur="{dur}" smil:fill="hold" smil:targetElement="{t}" smil:attributeName="y" smil:values="{fy};{ty}" smil:keyTimes="0;1" presentation:additive="base"/>"#
        )
    };
    // Zoom-like effects grow from (or shrink to) nothing.
    let size = |out: bool| {
        let (from, to) = if out { ("width", "0") } else { ("0", "width") };
        let (hf, ht) = if out { ("height", "0") } else { ("0", "height") };
        format!(
            r#"<anim:animate smil:dur="{dur}" smil:fill="hold" smil:targetElement="{t}" smil:attributeName="width" smil:values="{from};{to}" smil:keyTimes="0;1" presentation:additive="base"/><anim:animate smil:dur="{dur}" smil:fill="hold" smil:targetElement="{t}" smil:attributeName="height" smil:values="{hf};{ht}" smil:keyTimes="0;1" presentation:additive="base"/>"#
        )
    };
    match a.class {
        AnimClass::Entrance => {
            let body = match a.effect.as_str() {
                "appear" => String::new(),
                "fly" => fly(false),
                "zoom" | "expand" | "grow" => format!("{}{}", size(false), filter("fade", "crossfade", false)),
                _ => wipe.map(|(ty, sub)| filter(ty, &sub, false)).unwrap_or_else(|| filter("fade", "crossfade", false)),
            };
            format!("{show}{body}")
        }
        AnimClass::Exit => {
            let body = match a.effect.as_str() {
                "disappear" => String::new(),
                "flyOut" => fly(true),
                "zoomOut" | "contract" | "shrinkTurn" => format!("{}{}", size(true), filter("fade", "crossfade", true)),
                _ => wipe.map(|(ty, sub)| filter(ty, &sub, true)).unwrap_or_else(|| filter("fade", "crossfade", true)),
            };
            let hide = if a.effect == "disappear" { hide.replace(&format!(r#"smil:begin="{dur}""#), r#"smil:begin="0s""#) } else { hide };
            format!("{body}{hide}")
        }
        AnimClass::Emphasis => match a.effect.as_str() {
            "spin" => {
                let by = if a.option == "counterClockwise" { "-360" } else { "360" };
                format!(r#"<anim:animateTransform smil:dur="{dur}" smil:fill="hold" smil:targetElement="{t}" smil:by="{by}" svg:type="rotate"/>"#)
            }
            "growShrink" | "pulse" => {
                format!(r#"<anim:animateTransform smil:dur="{dur}" smil:fill="hold" smil:targetElement="{t}" smil:to="1.5,1.5" svg:type="scale"/>"#)
            }
            "transparency" => {
                format!(r#"<anim:set smil:dur="{dur}" smil:fill="hold" smil:targetElement="{t}" smil:attributeName="opacity" smil:to="0.5"/>"#)
            }
            "boldFlash" => {
                format!(r#"<anim:set smil:dur="{dur}" smil:fill="hold" smil:targetElement="{t}" smil:attributeName="charWeight" smil:to="bold"/>"#)
            }
            "underline" => format!(
                r#"<anim:set smil:dur="{dur}" smil:fill="hold" smil:targetElement="{t}" smil:attributeName="charUnderline" smil:to="solid"/>"#
            ),
            _ => format!(
                r#"<anim:animateTransform smil:dur="{dur}" smil:fill="hold" smil:targetElement="{t}" smil:to="1.1,1.1" svg:type="scale" smil:autoReverse="true"/>"#
            ),
        },
        AnimClass::Path => {
            let p = a.path.clone().or_else(|| deckcraft_model::anim::preset_path(&a.effect)).unwrap_or_else(|| "M 0 0 L 0.25 0".into());
            let p = p.trim_end().trim_end_matches('E').trim_end().to_string();
            format!(
                r#"<anim:animateMotion smil:dur="{dur}" smil:fill="hold" smil:targetElement="{t}" svg:path="{}" presentation:additive="sum"/>"#,
                esc(&p)
            )
        }
        AnimClass::Media => format!(r#"<anim:command smil:begin="0s" smil:targetElement="{t}" anim:command="play"/>"#),
    }
}

/// The timing root of a page: its transition and its main sequence.
pub fn write(page_id: &str, transition: Option<&str>, animations: &[Animation], ids: &dyn Fn(ShapeId, Option<u32>) -> Option<String>) -> String {
    let mut out = String::from(r#"<anim:par presentation:node-type="timing-root">"#);
    if let Some(t) = transition {
        let _ = write!(out, r#"<anim:par smil:begin="{page_id}.begin">{t}</anim:par>"#);
    }
    // Interactive sequences: per trigger shape.
    let (main, triggered): (Vec<&Animation>, Vec<&Animation>) = animations.iter().partition(|a| a.trigger.is_none());
    if !main.is_empty() {
        out.push_str(r#"<anim:seq smil:dur="indefinite" presentation:node-type="main-sequence">"#);
        out.push_str(&groups(&main, ids, "next"));
        out.push_str("</anim:seq>");
    }
    let mut by_trigger: Vec<(ShapeId, Vec<&Animation>)> = vec![];
    for a in triggered {
        let Some(tr) = a.trigger else { continue };
        match by_trigger.iter_mut().find(|(t, _)| *t == tr) {
            Some((_, v)) => v.push(a),
            None => by_trigger.push((tr, vec![a])),
        }
    }
    for (tr, list) in by_trigger {
        let Some(tid) = ids(tr, None) else { continue };
        let _ = write!(out, r#"<anim:seq smil:dur="indefinite" smil:begin="{tid}.click" presentation:node-type="interactive-sequence">"#);
        out.push_str(&groups(&list, ids, "0s"));
        out.push_str("</anim:seq>");
    }
    out.push_str("</anim:par>");
    out
}

/// Click groups of a sequence, each holding time groups of effects.
fn groups(list: &[&Animation], ids: &dyn Fn(ShapeId, Option<u32>) -> Option<String>, first_begin: &str) -> String {
    let mut out = String::new();
    let mut i = 0;
    let mut first = true;
    while i < list.len() {
        // A click group: an on-click effect (or the first effects) and everything after it until the
        // next on-click.
        let mut j = i + 1;
        while j < list.len() && list[j].start != AnimStart::OnClick {
            j += 1;
        }
        let begin = if first && list[i].start != AnimStart::OnClick {
            "0s"
        } else if first {
            first_begin
        } else {
            "next"
        };
        first = false;
        let _ = write!(out, r#"<anim:par smil:begin="{begin}" smil:fill="hold">"#);
        // Time groups: a new one at each after-previous effect, starting when the previous ends.
        let mut t0 = 0u32;
        let mut block_end = 0u32;
        let mut open = false;
        for (k, a) in list[i..j].iter().enumerate() {
            if k == 0 || a.start == AnimStart::AfterPrevious {
                if open {
                    out.push_str("</anim:par>");
                }
                t0 = if k == 0 { 0 } else { block_end };
                let _ = write!(out, r#"<anim:par smil:begin="{}s" smil:fill="hold">"#, f64::from(t0) / 1000.0);
                open = true;
            }
            let Some(t) = ids(a.shape, a.paragraph) else { continue };
            let (preset, sub) = preset_of(a);
            let node = match a.start {
                AnimStart::OnClick => "on-click",
                AnimStart::WithPrevious => "with-previous",
                AnimStart::AfterPrevious => "after-previous",
            };
            let class = match a.class {
                AnimClass::Entrance => "entrance",
                AnimClass::Exit => "exit",
                AnimClass::Emphasis => "emphasis",
                AnimClass::Path => "motion-path",
                AnimClass::Media => "media-call",
            };
            let sub = if sub.is_empty() { String::new() } else { format!(r#" presentation:preset-sub-type="{}""#, esc(&sub)) };
            let repeat = if a.repeat > 1 { format!(r#" smil:repeatCount="{}""#, a.repeat) } else { String::new() };
            let _ = write!(
                out,
                r#"<anim:par smil:begin="{}s" smil:fill="hold"{repeat} presentation:node-type="{node}" presentation:preset-class="{class}" presentation:preset-id="{}"{sub}>{}</anim:par>"#,
                f64::from(a.delay_ms) / 1000.0,
                esc(&preset),
                nodes(a, &t)
            );
            block_end = block_end.max(t0 + a.delay_ms + a.duration_ms);
        }
        if open {
            out.push_str("</anim:par>");
        }
        out.push_str("</anim:par>");
        i = j;
    }
    out
}

/// A transition as the `anim:transitionFilter` LibreOffice reads.
pub fn transition_filter(t: &Transition) -> Option<String> {
    let dir = |o: &str| match o {
        "l" => "fromRight",
        "u" => "fromBottom",
        "d" => "fromTop",
        _ => "fromLeft",
    };
    let (ty, sub, reverse) = match t.kind.as_str() {
        "fade" => ("fade", "crossfade", false),
        "push" => ("pushWipe", dir(&t.option), false),
        "cover" | "pull" | "uncover" => ("slideWipe", dir(&t.option), t.kind == "uncover"),
        "wipe" => {
            ("barWipe", if matches!(t.option.as_str(), "u" | "d") { "topToBottom" } else { "leftToRight" }, matches!(t.option.as_str(), "l" | "u"))
        }
        "split" => ("barnDoorWipe", if t.option.starts_with("horz") { "horizontal" } else { "vertical" }, false),
        "circle" | "shape" => ("ellipseWipe", "circle", false),
        "wheel" | "clock" => ("clockWipe", "clockwiseTwelve", false),
        "blinds" => ("blindsWipe", if t.option == "horz" { "horizontal" } else { "vertical" }, false),
        "checker" => ("checkerBoardWipe", "across", false),
        "randomBar" => ("randomBarWipe", if t.option == "horz" { "horizontal" } else { "vertical" }, false),
        "dissolve" => ("dissolve", "default", false),
        "zoom" => ("zoom", "rotateIn", false),
        "none" | "cut" => return None,
        _ => ("fade", "crossfade", false),
    };
    let direction = if reverse { r#" smil:direction="reverse""# } else { "" };
    Some(format!(r#"<anim:transitionFilter smil:dur="{}s" smil:type="{ty}" smil:subtype="{sub}"{direction}/>"#, f64::from(t.duration_ms) / 1000.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_map_both_ways() {
        for (class, preset, sub, effect, opt) in [
            (AnimClass::Entrance, "ooo-entrance-fly-in", "from-left", "fly", "l"),
            (AnimClass::Entrance, "ooo-entrance-fade-in", "", "fade", ""),
            (AnimClass::Exit, "ooo-exit-wipe", "from-top", "wipeOut", "t"),
            (AnimClass::Emphasis, "ooo-emphasis-spin", "", "spin", "clockwise"),
        ] {
            let (e, o) = effect_of(class, preset, sub);
            assert_eq!((e.as_str(), o.as_str()), (effect, opt));
            let a = Animation { class, effect: e, option: o, ..Default::default() };
            let (p, s) = preset_of(&a);
            assert_eq!(p, preset);
            assert_eq!(s, sub);
        }
    }
}
