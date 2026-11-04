//! Slide transitions and object animations in a page's timing tree (`anim:par` under
//! `presentation:node-type="timing-root"`), read into the model and written back as the SMIL
//! nodes LibreOffice plays.
//!
//! Every mapping between our ids and the file's is one table read in both directions: [`EFFECTS`]
//! for effect presets, [`OPTIONS`] for their sub-types and [`TRANSITIONS`] for transition filters.

use std::collections::HashMap;
use std::fmt::Write as _;

use deckcraft_color::Rgba;
use deckcraft_model::anim::{ANIMATIONS, AnimClass, AnimStart, Animation, BuildRole, TextBuild};
use deckcraft_model::resolve::Ctx;
use deckcraft_model::style::ColorRef;
use deckcraft_model::{ShapeId, Transition};

use crate::dom::{El, attr, esc, kids};
use crate::units::{clock, hex, trim};

/// Most effects read from one page.
const MAX_EFFECTS: usize = 2_000;

/// Longest effect, delay or transition read, in ms.
const MAX_MS: u32 = 600_000;

/// Each class: its `presentation:preset-class` and the infix of its preset ids
/// (`ooo-{infix}-{name}`).
const CLASSES: [(AnimClass, &str, &str); 5] = [
    (AnimClass::Entrance, "entrance", "entrance"),
    (AnimClass::Exit, "exit", "exit"),
    (AnimClass::Emphasis, "emphasis", "emphasis"),
    (AnimClass::Path, "motion-path", "motionpath"),
    (AnimClass::Media, "media-call", "media"),
];

/// (class, our effect id, LibreOffice preset name, other names read as it). The first row of a
/// class is what an unknown preset of that class reads as. Motion paths and media aren't here:
/// paths keep our id as the preset name, media is told by its command.
const EFFECTS: &[(AnimClass, &str, &str, &[&str])] = &[
    (AnimClass::Entrance, "fade", "fade-in", &["fade-in-and-swivel"]),
    (AnimClass::Entrance, "appear", "appear", &[]),
    (AnimClass::Entrance, "fly", "fly-in", &["fly-in-slow"]),
    (AnimClass::Entrance, "float", "float", &["float-in"]),
    (AnimClass::Entrance, "split", "split", &[]),
    (AnimClass::Entrance, "wipe", "wipe", &[]),
    (AnimClass::Entrance, "shape", "plus", &[]),
    (AnimClass::Entrance, "wheel", "wheel", &[]),
    (AnimClass::Entrance, "randomBars", "random-bars", &[]),
    (AnimClass::Entrance, "grow", "grow-and-turn", &[]),
    (AnimClass::Entrance, "zoom", "fade-in-and-zoom", &["zoom"]),
    (AnimClass::Entrance, "swivel", "swivel", &[]),
    (AnimClass::Entrance, "bounce", "boomerang", &["bounce"]),
    (AnimClass::Entrance, "blinds", "venetian-blinds", &[]),
    (AnimClass::Entrance, "box", "box", &[]),
    (AnimClass::Entrance, "checkerboard", "checkerboard", &[]),
    (AnimClass::Entrance, "circle", "circle", &[]),
    (AnimClass::Entrance, "diamond", "diamond", &[]),
    (AnimClass::Entrance, "dissolve", "dissolve-in", &[]),
    (AnimClass::Entrance, "peek", "peek-in", &[]),
    (AnimClass::Entrance, "strips", "diagonal-squares", &["strips"]),
    (AnimClass::Entrance, "expand", "expand", &[]),
    (AnimClass::Entrance, "rise", "ascend", &["rise-up"]),
    (AnimClass::Entrance, "spinner", "spinner", &["spin-in"]),
    (AnimClass::Exit, "fadeOut", "fade-out", &[]),
    (AnimClass::Exit, "disappear", "disappear", &[]),
    (AnimClass::Exit, "flyOut", "fly-out", &["fly-out-slow"]),
    (AnimClass::Exit, "floatOut", "float", &["float-out"]),
    (AnimClass::Exit, "splitOut", "split", &[]),
    (AnimClass::Exit, "wipeOut", "wipe", &[]),
    (AnimClass::Exit, "shapeOut", "circle", &[]),
    (AnimClass::Exit, "wheelOut", "wheel", &[]),
    (AnimClass::Exit, "randomBarsOut", "random-bars", &[]),
    (AnimClass::Exit, "shrinkTurn", "grow-and-turn", &["shrink-and-turn"]),
    (AnimClass::Exit, "zoomOut", "fade-out-and-zoom", &["zoom"]),
    (AnimClass::Exit, "swivelOut", "swivel", &[]),
    (AnimClass::Exit, "bounceOut", "boomerang", &["bounce"]),
    (AnimClass::Exit, "dissolveOut", "dissolve", &[]),
    (AnimClass::Exit, "blindsOut", "venetian-blinds", &[]),
    (AnimClass::Exit, "boxOut", "box", &[]),
    (AnimClass::Exit, "checkerboardOut", "checkerboard", &[]),
    (AnimClass::Exit, "diamondOut", "diamond", &[]),
    (AnimClass::Exit, "peekOut", "peek-out", &[]),
    (AnimClass::Exit, "stripsOut", "diagonal-squares", &["strips"]),
    (AnimClass::Exit, "sinkDown", "descend", &["sink-down"]),
    (AnimClass::Exit, "spinnerOut", "spinner", &["spin-out"]),
    (AnimClass::Exit, "contract", "contract", &[]),
    (AnimClass::Emphasis, "pulse", "pulse", &[]),
    (AnimClass::Emphasis, "colorPulse", "color-pulse", &[]),
    (AnimClass::Emphasis, "teeter", "teeter", &[]),
    (AnimClass::Emphasis, "spin", "spin", &[]),
    (AnimClass::Emphasis, "growShrink", "grow-and-shrink", &[]),
    (AnimClass::Emphasis, "desaturate", "desaturate", &[]),
    (AnimClass::Emphasis, "darken", "darken", &[]),
    (AnimClass::Emphasis, "lighten", "lighten", &[]),
    (AnimClass::Emphasis, "transparency", "transparency", &[]),
    (AnimClass::Emphasis, "objectColor", "object-color", &[]),
    (AnimClass::Emphasis, "complementaryColor", "complementary-color", &[]),
    (AnimClass::Emphasis, "lineColor", "line-color", &[]),
    (AnimClass::Emphasis, "fillColor", "fill-color", &[]),
    (AnimClass::Emphasis, "fontColor", "font-color", &["color-blend", "color-over-by-word", "color-over-by-letter"]),
    (AnimClass::Emphasis, "underline", "underline", &[]),
    (AnimClass::Emphasis, "boldFlash", "bold-flash", &[]),
    (AnimClass::Emphasis, "wave", "wave", &[]),
];

/// Effects whose option names a shape and a direction (`circleIn`): the preset names the shape,
/// the sub-type the direction.
const SHAPE_OPTIONS: [(AnimClass, &str, &str); 4] = [
    (AnimClass::Entrance, "shape", "plus"),
    (AnimClass::Entrance, "circle", "circle"),
    (AnimClass::Entrance, "box", "box"),
    (AnimClass::Entrance, "diamond", "diamond"),
];

/// (our option, LibreOffice preset sub-type). Options not listed are written as they are.
const OPTIONS: &[(&str, &str)] = &[
    ("b", "from-bottom"),
    ("t", "from-top"),
    ("l", "from-left"),
    ("r", "from-right"),
    ("lb", "from-bottom-left"),
    ("lt", "from-top-left"),
    ("rt", "from-top-right"),
    ("rb", "from-bottom-right"),
    ("horz", "horizontal"),
    ("vert", "vertical"),
    ("down", "downward"),
    ("horzIn", "horizontal-in"),
    ("horzOut", "horizontal-out"),
    ("vertIn", "vertical-in"),
    ("vertOut", "vertical-out"),
    ("lu", "left-to-top"),
    ("ru", "right-to-top"),
    ("ld", "left-to-bottom"),
    ("rd", "right-to-bottom"),
];

/// Media effects and the `anim:command` that plays them.
const MEDIA: [(&str, &str); 3] = [("play", "play"), ("pause", "toggle-pause"), ("stop", "stop")];

/// (transition kind, option, `smil:type`, `smil:subtype`, reversed). Moving transitions store the
/// direction things move, while ODF names where the new slide comes from: Push and Cover `l` come
/// from the right. Uncover is a reversed slide: the old slide leaves towards its option. Reading
/// takes the exact row, else the first row of the type; writing the exact row, else the first of
/// the kind.
pub(crate) const TRANSITIONS: &[(&str, &str, &str, &str, bool)] = &[
    ("fade", "", "fade", "crossfade", false),
    ("fade", "throughBlack", "fade", "fadeOverColor", false),
    ("push", "u", "pushWipe", "fromBottom", false),
    ("push", "d", "pushWipe", "fromTop", false),
    ("push", "l", "pushWipe", "fromRight", false),
    ("push", "r", "pushWipe", "fromLeft", false),
    ("cover", "l", "slideWipe", "fromRight", false),
    ("cover", "r", "slideWipe", "fromLeft", false),
    ("cover", "u", "slideWipe", "fromBottom", false),
    ("cover", "d", "slideWipe", "fromTop", false),
    ("cover", "lu", "slideWipe", "fromBottomRight", false),
    ("cover", "ru", "slideWipe", "fromBottomLeft", false),
    ("cover", "ld", "slideWipe", "fromTopRight", false),
    ("cover", "rd", "slideWipe", "fromTopLeft", false),
    ("uncover", "l", "slideWipe", "fromLeft", true),
    ("uncover", "r", "slideWipe", "fromRight", true),
    ("uncover", "u", "slideWipe", "fromTop", true),
    ("uncover", "d", "slideWipe", "fromBottom", true),
    ("uncover", "lu", "slideWipe", "fromTopLeft", true),
    ("uncover", "ru", "slideWipe", "fromTopRight", true),
    ("uncover", "ld", "slideWipe", "fromBottomLeft", true),
    ("uncover", "rd", "slideWipe", "fromBottomRight", true),
    ("wipe", "r", "barWipe", "leftToRight", false),
    ("wipe", "l", "barWipe", "leftToRight", true),
    ("wipe", "d", "barWipe", "topToBottom", false),
    ("wipe", "u", "barWipe", "topToBottom", true),
    ("split", "vertOut", "barnDoorWipe", "vertical", false),
    ("split", "vertIn", "barnDoorWipe", "vertical", true),
    ("split", "horzOut", "barnDoorWipe", "horizontal", false),
    ("split", "horzIn", "barnDoorWipe", "horizontal", true),
    ("randomBar", "vert", "randomBarWipe", "vertical", false),
    ("randomBar", "horz", "randomBarWipe", "horizontal", false),
    ("shape", "circle", "ellipseWipe", "circle", false),
    ("shape", "in", "ellipseWipe", "circle", true),
    ("shape", "diamond", "irisWipe", "diamond", false),
    ("dissolve", "", "dissolve", "default", false),
    ("checker", "horz", "checkerBoardWipe", "across", false),
    ("checker", "vert", "checkerBoardWipe", "down", false),
    ("blinds", "vert", "blindsWipe", "vertical", false),
    ("blinds", "horz", "blindsWipe", "horizontal", false),
    ("clock", "clockwise", "clockWipe", "clockwiseTwelve", false),
    ("clock", "counterClockwise", "clockWipe", "clockwiseTwelve", true),
    ("clock", "wedge", "fanWipe", "centerTop", false),
    ("zoom", "in", "zoom", "rotateIn", false),
];

/// Our transition kind and option for a SMIL transition filter.
pub fn transition_of(ty: &str, sub: &str, reverse: bool) -> (&'static str, &'static str) {
    TRANSITIONS
        .iter()
        .find(|r| r.2 == ty && r.3 == sub && r.4 == reverse)
        .or_else(|| TRANSITIONS.iter().find(|r| r.2 == ty))
        .map_or(("fade", ""), |r| (r.0, r.1))
}

/// The SMIL type, sub-type and direction of a transition; `None` when nothing plays.
pub fn transition_smil(t: &Transition) -> Option<(&'static str, &'static str, bool)> {
    if matches!(t.kind.as_str(), "none" | "cut") {
        return None;
    }
    TRANSITIONS
        .iter()
        .find(|r| r.0 == t.kind && r.1 == t.option)
        .or_else(|| TRANSITIONS.iter().find(|r| r.0 == t.kind))
        .or(TRANSITIONS.first())
        .map(|r| (r.2, r.3, r.4))
}

/// The SMIL attributes of a transition, as a page style or a transition filter carries them.
fn smil_attrs(t: &Transition) -> Option<String> {
    let (ty, sub, reverse) = transition_smil(t)?;
    let direction = if reverse { r#" smil:direction="reverse""# } else { "" };
    let color = if sub == "fadeOverColor" { r##" smil:fadeColor="#000000""## } else { "" };
    Some(format!(r#" smil:type="{ty}" smil:subtype="{sub}"{direction}{color} smil:dur="{}s""#, secs(t.duration_ms)))
}

/// A transition as the drawing-page style properties ODF 1.2+ readers take it from.
pub fn transition_props(t: &Transition) -> String {
    let mut s = smil_attrs(t).unwrap_or_default();
    if let Some(ms) = t.advance_after_ms {
        let _ = write!(s, r#" presentation:transition-type="automatic" presentation:duration="PT{}S""#, secs(ms));
    }
    s
}

/// A transition as the `anim:transitionFilter` LibreOffice plays.
pub fn transition_filter(t: &Transition) -> Option<String> {
    smil_attrs(t).map(|a| format!("<anim:transitionFilter{a}/>"))
}

fn secs(ms: u32) -> String {
    trim(f64::from(ms) / 1000.0, 3)
}

/// The transition a page style describes: SMIL attributes, or the ODF 1.0 `transition-style`.
pub fn page_transition(pp: &crate::styles::Props) -> Option<deckcraft_model::Transition> {
    let get = |k: &str| pp.get(k).map(String::as_str);
    let (kind, option) = match (get("smil:type"), get("presentation:transition-style")) {
        (Some(ty), _) => transition_of(ty, get("smil:subtype").unwrap_or(""), get("smil:direction") == Some("reverse")),
        (None, Some(s)) if s.contains("fade") => ("fade", ""),
        (None, Some(s)) if s.starts_with("move") => ("cover", ""),
        (None, Some(s)) if s != "none" => ("fade", ""),
        _ if pp.contains_key("presentation:duration") => ("none", ""),
        _ => return None,
    };
    let duration = get("smil:dur")
        .and_then(clock)
        .or_else(|| match get("presentation:transition-speed")? {
            "slow" => Some(1000),
            "fast" => Some(300),
            _ => Some(500),
        })
        .unwrap_or(700);
    let auto = get("presentation:transition-type") == Some("automatic");
    let after = if auto { get("presentation:duration").and_then(duration_iso) } else { None };
    if kind == "none" && after.is_none() {
        return None;
    }
    Some(deckcraft_model::Transition {
        kind: kind.into(),
        option: option.into(),
        duration_ms: duration.min(60_000),
        advance_after_ms: after,
        ..Default::default()
    })
}

/// An ISO 8601 duration (`PT00H00M05S`, `PT5.5S`) in ms.
fn duration_iso(s: &str) -> Option<u32> {
    let t = s.trim().strip_prefix("PT").or_else(|| s.trim().strip_prefix("P0DT"))?;
    let mut total = 0.0;
    let mut num = String::new();
    for c in t.chars() {
        match c {
            'H' => total += num.parse::<f64>().ok()? * 3_600_000.0,
            'M' => total += num.parse::<f64>().ok()? * 60_000.0,
            'S' => total += num.parse::<f64>().ok()? * 1000.0,
            _ => {
                num.push(c);
                continue;
            }
        }
        num.clear();
    }
    (total.is_finite() && total >= 0.0).then(|| total.min(f64::from(u32::MAX)) as u32)
}

/// Our effect id and option for a LibreOffice preset id and sub-type.
fn effect_of(class: AnimClass, preset: &str, sub: &str) -> (String, String) {
    let infix = CLASSES.iter().find(|c| c.0 == class).map_or("", |c| c.2);
    let name = preset.strip_prefix("ooo-").and_then(|p| p.strip_prefix(infix)).and_then(|p| p.strip_prefix('-')).unwrap_or(preset);
    let option = OPTIONS.iter().find(|o| o.1 == sub).map_or(sub, |o| o.0);
    match class {
        AnimClass::Path => {
            let ours = ANIMATIONS.iter().any(|a| a.2 == AnimClass::Path && a.0 == name);
            ((if ours { name } else { "customPath" }).to_string(), option.to_string())
        }
        AnimClass::Media => (MEDIA.iter().find(|m| m.0 == name).map_or("play", |m| m.0).into(), String::new()),
        _ => {
            let row = EFFECTS.iter().filter(|e| e.0 == class);
            let row = row.clone().find(|e| e.2 == name).or_else(|| row.clone().find(|e| e.3.contains(&name))).or_else(|| row.clone().next());
            let effect = row.map_or("fade", |e| e.1);
            match SHAPE_OPTIONS.iter().find(|s| s.0 == class && s.1 == effect) {
                // `shape` + `in` → `plusIn`: the shape goes back into the option.
                Some((_, "shape", shape)) => ("shape".into(), format!("{shape}{}", capitalized(option))),
                _ if effect == "shapeOut" => (effect.into(), format!("circle{}", capitalized(option))),
                _ => (effect.into(), option.into()),
            }
        }
    }
}

/// An effect as LibreOffice names it: the shape effects carry their shape in the effect
/// (`shape` + `circleIn` is the circle, `in`; an exit shape is always the circle).
fn canonical(a: &Animation) -> (&str, String) {
    if a.effect != "shape" && a.effect != "shapeOut" {
        return (a.effect.as_str(), a.option.clone());
    }
    let split = SHAPE_OPTIONS.iter().find_map(|s| a.option.strip_prefix(s.2).map(|rest| (s.1, rest)));
    let (shape, dir) = split.unwrap_or(("circle", a.option.as_str()));
    (if a.class == AnimClass::Exit { "shapeOut" } else { shape }, dir.to_ascii_lowercase())
}

/// The LibreOffice preset id and sub-type for our effect.
fn preset_of(a: &Animation) -> (String, String) {
    let infix = CLASSES.iter().find(|c| c.0 == a.class).map_or("entrance", |c| c.2);
    let (effect, option) = canonical(a);
    let name = match a.class {
        AnimClass::Path if effect != "customPath" && ANIMATIONS.iter().any(|x| x.2 == AnimClass::Path && x.0 == effect) => effect,
        AnimClass::Path => "custom",
        AnimClass::Media => MEDIA.iter().find(|m| m.0 == effect).map_or("play", |m| m.0),
        class => {
            let row = EFFECTS.iter().find(|e| e.0 == class && e.1 == effect).or_else(|| EFFECTS.iter().find(|e| e.0 == class));
            row.map_or("fade-in", |e| e.2)
        }
    };
    let sub = if effect == "spin" { "" } else { OPTIONS.iter().find(|o| o.0 == option).map_or(option.as_str(), |o| o.1) };
    (format!("ooo-{infix}-{name}"), sub.to_string())
}

fn capitalized(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_ascii_uppercase().to_string() + c.as_str()).unwrap_or_default()
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
                    let (kind, option) = transition_of(
                        attr(f, "smil:type").unwrap_or("fade"),
                        attr(f, "smil:subtype").unwrap_or(""),
                        attr(f, "smil:direction") == Some("reverse"),
                    );
                    t.transition = Some(Transition {
                        kind: kind.into(),
                        option: option.into(),
                        duration_ms: attr(f, "smil:dur").and_then(clock).unwrap_or(700).min(60_000),
                        ..Default::default()
                    });
                }
            }
            _ => {}
        }
    }
    t
}

fn read_sequence(seq: &El, trigger: Option<ShapeId>, targets: &Targets, out: &mut Vec<Animation>) {
    // Click groups → time groups → effects, each with the text group (`presentation:group-id`)
    // it belongs to.
    let mut read: Vec<(Animation, Option<&str>)> = vec![];
    'all: for group in kids(seq, "anim:par") {
        for timed in kids(group, "anim:par") {
            for effect in kids(timed, "anim:par").chain(kids(timed, "anim:iterate")) {
                if out.len() + read.len() >= MAX_EFFECTS {
                    break 'all;
                }
                if let Some(a) = read_effect(effect, trigger, targets) {
                    read.push((a, attr(effect, "presentation:group-id")));
                }
            }
        }
    }
    out.extend(join_builds(read));
}

/// Consecutive effects of one text group on paragraphs of one shape are read as one effect on the
/// whole shape: all together (every later one with the first) as one block, started the way
/// [`expand_builds`] starts a build's paragraphs (each on a click, or each after the one before;
/// deeper ones with their parent) as that build. A group started otherwise, and a lone effect,
/// stay as they are.
fn join_builds(read: Vec<(Animation, Option<&str>)>) -> Vec<Animation> {
    let mut out: Vec<Animation> = vec![];
    let mut i = 0;
    while let Some((a, group)) = read.get(i) {
        let grouped = group.is_some() && a.paragraph.is_some();
        let same = |k: usize| read.get(k).is_some_and(|(b, g)| g == group && b.shape == a.shape && b.paragraph.is_some());
        let mut end = i + 1;
        while grouped && same(end) {
            end += 1;
        }
        let next = if a.start == AnimStart::OnClick { AnimStart::OnClick } else { AnimStart::AfterPrevious };
        let rest = read.get(i + 1..end).unwrap_or_default();
        let together = rest.iter().all(|(b, _)| b.start == AnimStart::WithPrevious);
        let as_built = rest.iter().all(|(b, _)| b.start == next || b.start == AnimStart::WithPrevious);
        if !rest.is_empty() && (together || as_built) {
            let mut first = a.clone();
            first.paragraph = None;
            first.text_build = if together { TextBuild::AsOne } else { TextBuild::ByParagraph };
            out.push(first);
        } else {
            out.extend(read.get(i..end).unwrap_or_default().iter().map(|(b, _)| b.clone()));
        }
        i = end;
    }
    out
}

/// A by-paragraph build as LibreOffice plays one: an effect per paragraph shown on its own
/// (deeper and blank paragraphs come with the one above, as PowerPoint plays them), each marked
/// with its build's text group. Other effects are written as they are.
pub fn expand_builds(animations: &[Animation], shapes: &[deckcraft_model::Shape]) -> Vec<Effect> {
    let mut out = vec![];
    for (n, a) in animations.iter().enumerate() {
        let text = deckcraft_model::find_shape(shapes, a.shape).and_then(|s| s.text.as_ref()).filter(|_| a.text_build == TextBuild::ByParagraph);
        let Some(text) = text else {
            out.push(Effect { a: a.clone(), build: None });
            continue;
        };
        let mut tops = 0;
        let effects: Vec<Effect> = deckcraft_model::anim::paragraph_build_roles(text)
            .iter()
            .enumerate()
            .filter_map(|(i, role)| {
                let start = match role {
                    BuildRole::Top => {
                        tops += 1;
                        match (tops, a.start) {
                            (1, s) | (_, s @ AnimStart::OnClick) => s,
                            _ => AnimStart::AfterPrevious,
                        }
                    }
                    BuildRole::Member(_) if text.paragraphs.get(i).is_some_and(deckcraft_model::anim::build_filled) => AnimStart::WithPrevious,
                    _ => return None,
                };
                let paragraph = u32::try_from(i).ok();
                Some(Effect { a: Animation { start, paragraph, text_build: TextBuild::AsOne, ..a.clone() }, build: Some(n) })
            })
            .collect();
        // A build of one paragraph is an effect on the whole shape; so is one that doesn't fit the
        // page's effect budget whole (rather than just its first part).
        let limit = deckcraft_model::anim::MAX_BUILD_PARAGRAPHS.min(MAX_EFFECTS.saturating_sub(out.len()));
        if effects.len() <= 1 || effects.len() > limit {
            out.push(Effect { a: Animation { text_build: TextBuild::AsOne, ..a.clone() }, build: None });
        } else {
            out.extend(effects);
        }
    }
    out
}

/// An effect as written: its animation, and the by-paragraph build it is part of.
pub struct Effect {
    pub a: Animation,
    pub build: Option<usize>,
}

/// What an effect's animation nodes do.
#[derive(Default)]
struct Scan {
    /// The first `smil:targetElement`.
    target: Option<String>,
    /// The longest animation, in ms.
    dur: u32,
    path: Option<String>,
    /// Rotation (`smil:by`), scale (`smil:to`), opacity and colour reached.
    rotate: Option<f64>,
    scale: Option<(f64, f64)>,
    opacity: Option<f64>,
    color: Option<Rgba>,
    command: Option<String>,
}

impl Scan {
    fn visit(&mut self, el: &El, depth: usize) {
        if depth > 16 {
            return;
        }
        for e in el.elements() {
            if self.target.is_none() {
                self.target = attr(e, "smil:targetElement").map(str::to_string);
            }
            let num = |k: &str| attr(e, k).and_then(|v| v.trim().parse::<f64>().ok()).filter(|v| v.is_finite());
            match (e.name.as_str(), attr(e, "svg:type")) {
                ("anim:animateMotion", _) if self.path.is_none() => self.path = attr(e, "svg:path").map(str::to_string),
                ("anim:animateTransform", Some("rotate")) => self.rotate = self.rotate.or(num("smil:by")),
                ("anim:animateTransform", Some("scale")) => {
                    let to = attr(e, "smil:to").and_then(|v| v.split_once(','));
                    self.scale = self.scale.or(to.and_then(|(x, y)| Some((x.trim().parse().ok()?, y.trim().parse().ok()?))));
                }
                ("anim:animateColor", _) => self.color = self.color.or(attr(e, "smil:to").and_then(crate::units::color)),
                ("anim:set" | "anim:animate", _) if attr(e, "smil:attributeName") == Some("opacity") => {
                    self.opacity = self.opacity.or(num("smil:to"));
                }
                ("anim:command", _) => self.command = self.command.take().or(attr(e, "anim:command").map(str::to_string)),
                _ => {}
            }
            // The 1 ms visibility switch isn't the effect's length.
            if let Some(d) = attr(e, "smil:dur").and_then(clock).filter(|d| *d > 1) {
                self.dur = self.dur.max(d);
            }
            self.visit(e, depth + 1);
        }
    }
}

fn read_effect(e: &El, trigger: Option<ShapeId>, targets: &Targets) -> Option<Animation> {
    let class = attr(e, "presentation:preset-class").and_then(|c| CLASSES.iter().find(|k| k.1 == c))?.0;
    let mut scan = Scan { target: attr(e, "smil:targetElement").map(str::to_string), ..Default::default() };
    scan.visit(e, 0);
    let (shape, paragraph) = *targets.get(scan.target.as_deref()?)?;
    let (mut effect, mut option) =
        effect_of(class, attr(e, "presentation:preset-id").unwrap_or(""), attr(e, "presentation:preset-sub-type").unwrap_or(""));
    let mut amount = None;
    match effect.as_str() {
        "spin" => {
            let by = scan.rotate.unwrap_or(360.0);
            option = if by < 0.0 { "counterClockwise" } else { "clockwise" }.into();
            amount = Some(by.abs().min(36_000.0)).filter(|a| (a - 360.0).abs() > 1e-6);
        }
        "growShrink" => {
            let (x, y) = scan.scale.unwrap_or((1.5, 1.5));
            let one = |v: f64| (v - 1.0).abs() < 1e-6;
            if one(y) && !one(x) {
                option = "horz".into();
            } else if one(x) && !one(y) {
                option = "vert".into();
            }
            let s = if one(x) { y } else { x };
            amount = Some(s.clamp(0.0, 100.0)).filter(|a| (a - 1.5).abs() > 1e-6);
        }
        "transparency" => amount = scan.opacity.map(|o| (1.0 - o).clamp(0.0, 1.0)).filter(|a| (a - 0.5).abs() > 1e-6),
        _ => {}
    }
    // What a media effect does is its command; the preset only names it.
    if let Some(cmd) = scan.command.as_deref().filter(|_| class == AnimClass::Media) {
        effect = MEDIA.iter().find(|m| m.1 == cmd).map_or("play", |m| m.0).into();
    }
    let start = match attr(e, "presentation:node-type") {
        Some("with-previous") => AnimStart::WithPrevious,
        Some("after-previous") => AnimStart::AfterPrevious,
        _ => AnimStart::OnClick,
    };
    // Timing modifiers sit on the effect node; older files put them on its first animation.
    let modifier = |k: &str| attr(e, k).or_else(|| e.elements().find_map(|c| attr(c, k)));
    let fraction = |k: &str| modifier(k).and_then(|v| v.trim().parse::<f64>().ok()).filter(|v| v.is_finite()).map_or(0.0, |v| v.clamp(0.0, 1.0));
    let repeat = match modifier("smil:repeatCount").map(str::trim) {
        Some("indefinite") => u32::MAX,
        Some(r) => r.parse::<f64>().ok().filter(|r| r.is_finite()).map_or(1, |r| r.round().clamp(1.0, 1000.0) as u32),
        None => 1,
    };
    let fixed_length = matches!(class, AnimClass::Entrance | AnimClass::Exit | AnimClass::Media);
    let mut a = Animation {
        shape,
        class,
        effect,
        option,
        start,
        duration_ms: if scan.dur == 0 && !fixed_length { 500 } else { scan.dur }.min(MAX_MS),
        delay_ms: attr(e, "smil:begin").and_then(clock).unwrap_or(0).min(MAX_MS),
        repeat,
        rewind: attr(e, "smil:fill") == Some("remove"),
        auto_reverse: modifier("smil:autoReverse") == Some("true"),
        smooth_start: fraction("smil:accelerate"),
        smooth_end: fraction("smil:decelerate"),
        paragraph,
        trigger,
        amount,
        color: scan.color.filter(|_| class == AnimClass::Emphasis).map(ColorRef::rgb),
        ..Default::default()
    };
    if let Some(p) = scan.path.filter(|_| class == AnimClass::Path) {
        a.path = Some(if p.trim_end().ends_with('E') { p } else { format!("{} E", p.trim_end()) });
    }
    Some(a)
}

// ---------------------------------------------------------------------------------------------
// Writing

/// The SMIL nodes that make LibreOffice play one effect on `t`.
fn nodes(a: &Animation, t: &str, ctx: &Ctx) -> String {
    let dur = format!("{}s", secs(a.duration_ms.max(1)));
    let set = |begin: &str, to: &str| {
        format!(
            r#"<anim:set smil:begin="{begin}" smil:dur="0.001s" smil:fill="hold" smil:targetElement="{t}" smil:attributeName="visibility" smil:to="{to}"/>"#
        )
    };
    let filter = |ty: &str, sub: &str, reverse: bool, out: bool| {
        let direction = if reverse { r#" smil:direction="reverse""# } else { "" };
        let mode = if out { r#" smil:mode="out""# } else { "" };
        format!(
            r#"<anim:transitionFilter smil:dur="{dur}" smil:fill="hold" smil:targetElement="{t}" smil:type="{ty}" smil:subtype="{sub}"{direction}{mode}/>"#
        )
    };
    let animate = |name: &str, from: &str, to: &str| {
        format!(
            r#"<anim:animate smil:dur="{dur}" smil:fill="hold" smil:targetElement="{t}" smil:attributeName="{name}" smil:values="{from};{to}" smil:keyTimes="0;1" presentation:additive="base"/>"#
        )
    };
    let (effect, option) = canonical(a);
    let o = option.as_str();
    let out = a.class == AnimClass::Exit;
    // The entrance and exit body: what moves, grows or wipes the shape in or out.
    let body = || -> String {
        let base = effect.strip_suffix("Out").unwrap_or(effect);
        let crossfade = filter("fade", "crossfade", false, out);
        match base {
            "appear" | "disappear" => String::new(),
            "fly" => {
                let x = match o {
                    "l" | "lt" | "lb" => "0-width/2",
                    "r" | "rt" | "rb" => "1+width/2",
                    _ => "x",
                };
                let y = match o {
                    "t" | "lt" | "rt" => "0-height/2",
                    "l" | "r" => "y",
                    _ => "1+height/2",
                };
                let (fx, tx, fy, ty) = if out { ("x", x, "y", y) } else { (x, "x", y, "y") };
                animate("x", fx, tx) + &animate("y", fy, ty)
            }
            "zoom" | "expand" | "grow" | "contract" | "shrinkTurn" => {
                let (w0, w1, h0, h1) = if out { ("width", "0", "height", "0") } else { ("0", "width", "0", "height") };
                animate("width", w0, w1) + &animate("height", h0, h1) + &crossfade
            }
            "wipe" => match o {
                "l" | "r" => filter("barWipe", "leftToRight", o == "r", out),
                _ => filter("barWipe", "topToBottom", o != "t", out),
            },
            "blinds" => filter("blindsWipe", if o == "vert" { "vertical" } else { "horizontal" }, false, out),
            "box" => filter("irisWipe", "rectangle", o == "in", out),
            "checkerboard" => filter("checkerBoardWipe", if o == "down" { "down" } else { "across" }, false, out),
            "circle" | "shape" => filter("ellipseWipe", "circle", o == "in", out),
            "diamond" => filter("irisWipe", "diamond", o == "in", out),
            "randomBars" => filter("randomBarWipe", if o == "vert" { "vertical" } else { "horizontal" }, false, out),
            "split" => filter("barnDoorWipe", if o.starts_with("vert") { "vertical" } else { "horizontal" }, o.ends_with("In"), out),
            "wheel" => {
                let sub = match o {
                    "2" => "twoBladeVertical",
                    "3" => "threeBlade",
                    "4" => "fourBlade",
                    "8" => "eightBlade",
                    _ => "oneBlade",
                };
                filter("pinWheelWipe", sub, false, out)
            }
            "strips" => filter("waterfallWipe", "horizontalLeft", false, out),
            _ => crossfade,
        }
    };
    let fill_hold = format!(r#"smil:dur="{dur}" smil:fill="hold" smil:targetElement="{t}""#);
    let color = |name: &str| {
        let c = a.color.as_ref().map_or(Rgba::rgb(0xff, 0, 0), |c| ctx.color(c, None));
        format!(r#"<anim:animateColor {fill_hold} smil:attributeName="{name}" smil:to="{}" anim:color-interpolation="rgb"/>"#, hex(c))
    };
    match a.class {
        AnimClass::Entrance => set("0s", "visible") + &body(),
        // A disappearing shape hides at once; the others once their effect has played.
        AnimClass::Exit => body() + &set(if a.effect == "disappear" { "0s" } else { &dur }, "hidden"),
        AnimClass::Emphasis => match a.effect.as_str() {
            "spin" => {
                let by = a.amount.unwrap_or(360.0) * if o == "counterClockwise" { -1.0 } else { 1.0 };
                format!(r#"<anim:animateTransform {fill_hold} smil:by="{}" svg:type="rotate"/>"#, trim(by, 3))
            }
            "growShrink" => {
                let s = trim(a.amount.unwrap_or(1.5), 3);
                let to = match o {
                    "horz" => format!("{s},1"),
                    "vert" => format!("1,{s}"),
                    _ => format!("{s},{s}"),
                };
                format!(r#"<anim:animateTransform {fill_hold} smil:to="{to}" svg:type="scale"/>"#)
            }
            "transparency" => {
                let opacity = trim(1.0 - a.amount.unwrap_or(0.5).clamp(0.0, 1.0), 3);
                format!(r#"<anim:set {fill_hold} smil:attributeName="opacity" smil:to="{opacity}"/>"#)
            }
            "fillColor" => color("fillColor"),
            "lineColor" => color("lineColor"),
            "fontColor" => color("charColor"),
            "objectColor" => color("fillColor") + &color("lineColor"),
            "boldFlash" => format!(r#"<anim:set {fill_hold} smil:attributeName="charWeight" smil:to="bold"/>"#),
            "underline" => format!(r#"<anim:set {fill_hold} smil:attributeName="charUnderline" smil:to="solid"/>"#),
            _ => format!(r#"<anim:animateTransform {fill_hold} smil:to="1.1,1.1" svg:type="scale" smil:autoReverse="true"/>"#),
        },
        AnimClass::Path => {
            let p = a.path.clone().or_else(|| deckcraft_model::anim::preset_path(&a.effect)).unwrap_or_else(|| "M 0 0 L 0.25 0".into());
            let p = p.trim_end().trim_end_matches('E').trim_end();
            format!(r#"<anim:animateMotion {fill_hold} svg:path="{}" presentation:additive="sum"/>"#, esc(p))
        }
        AnimClass::Media => {
            let cmd = MEDIA.iter().find(|m| m.0 == a.effect).map_or("play", |m| m.1);
            format!(r#"<anim:command smil:begin="0s" smil:targetElement="{t}" anim:command="{cmd}"/>"#)
        }
    }
}

/// The timing root of a page: its transition, its main sequence and one interactive sequence per
/// trigger shape.
pub fn write(
    page_id: &str,
    transition: Option<&str>,
    animations: &[Effect],
    ctx: &Ctx,
    ids: &dyn Fn(ShapeId, Option<u32>) -> Option<String>,
) -> String {
    let mut out = String::from(r#"<anim:par presentation:node-type="timing-root">"#);
    if let Some(t) = transition {
        let _ = write!(out, r#"<anim:par smil:begin="{page_id}.begin">{t}</anim:par>"#);
    }
    let main: Vec<&Effect> = animations.iter().filter(|e| e.a.trigger.is_none()).collect();
    if !main.is_empty() {
        out.push_str(r#"<anim:seq smil:dur="indefinite" presentation:node-type="main-sequence">"#);
        groups(&mut out, &main, ctx, ids, "next");
        out.push_str("</anim:seq>");
    }
    let mut triggers: Vec<ShapeId> = vec![];
    for tr in animations.iter().filter_map(|e| e.a.trigger) {
        if !triggers.contains(&tr) {
            triggers.push(tr);
        }
    }
    for tr in triggers {
        let Some(tid) = ids(tr, None) else { continue };
        let list: Vec<&Effect> = animations.iter().filter(|e| e.a.trigger == Some(tr)).collect();
        let _ = write!(out, r#"<anim:seq smil:dur="indefinite" smil:begin="{tid}.click" presentation:node-type="interactive-sequence">"#);
        groups(&mut out, &list, ctx, ids, "0s");
        out.push_str("</anim:seq>");
    }
    out.push_str("</anim:par>");
    out
}

/// How long an effect runs from its group's start, repeats and reversal included.
fn span(a: &Animation) -> u32 {
    let once = a.duration_ms.saturating_mul(if a.auto_reverse { 2 } else { 1 });
    let times = if a.repeat == u32::MAX { 1 } else { a.repeat.max(1) };
    a.delay_ms.saturating_add(once.saturating_mul(times))
}

/// Click groups of a sequence, each holding time groups of effects.
fn groups(out: &mut String, list: &[&Effect], ctx: &Ctx, ids: &dyn Fn(ShapeId, Option<u32>) -> Option<String>, first_begin: &str) {
    // A click group starts at an on-click effect (or the sequence's first effect) and runs until
    // the next on-click one.
    let mut starts: Vec<usize> = (0..list.len()).filter(|&k| k == 0 || list[k].a.start == AnimStart::OnClick).collect();
    starts.push(list.len());
    for (gi, w) in starts.windows(2).enumerate() {
        let group = &list[w[0]..w[1]];
        let begin = match (gi, group.first().map(|e| e.a.start)) {
            (0, Some(AnimStart::OnClick)) => first_begin,
            (0, _) => "0s",
            _ => "next",
        };
        let _ = write!(out, r#"<anim:par smil:begin="{begin}" smil:fill="hold">"#);
        // Time groups: a new one at each after-previous effect, starting when the previous ends.
        let mut block_end = 0u32;
        let mut t0 = 0u32;
        for (k, e) in group.iter().enumerate() {
            let a = &e.a;
            if k == 0 || a.start == AnimStart::AfterPrevious {
                if k > 0 {
                    out.push_str("</anim:par>");
                    t0 = block_end;
                }
                let _ = write!(out, r#"<anim:par smil:begin="{}s" smil:fill="hold">"#, secs(t0));
            }
            block_end = block_end.max(t0.saturating_add(span(a)));
            let Some(t) = ids(a.shape, a.paragraph) else { continue };
            effect(out, a, e.build, &t, ctx);
        }
        if !group.is_empty() {
            out.push_str("</anim:par>");
        }
        out.push_str("</anim:par>");
    }
}

/// One effect node: its preset, timing modifiers and animations.
fn effect(out: &mut String, a: &Animation, build: Option<usize>, t: &str, ctx: &Ctx) {
    let (preset, sub) = preset_of(a);
    let node = match a.start {
        AnimStart::OnClick => "on-click",
        AnimStart::WithPrevious => "with-previous",
        AnimStart::AfterPrevious => "after-previous",
    };
    let class = CLASSES.iter().find(|c| c.0 == a.class).map_or("entrance", |c| c.1);
    let fill = if a.rewind { "remove" } else { "hold" };
    let _ = write!(out, r#"<anim:par smil:begin="{}s" smil:fill="{fill}""#, secs(a.delay_ms));
    match a.repeat {
        u32::MAX => out.push_str(r#" smil:repeatCount="indefinite""#),
        r if r > 1 => {
            let _ = write!(out, r#" smil:repeatCount="{r}""#);
        }
        _ => {}
    }
    if a.auto_reverse {
        out.push_str(r#" smil:autoReverse="true""#);
    }
    for (k, v) in [("smil:accelerate", a.smooth_start), ("smil:decelerate", a.smooth_end)] {
        if v > 0.0 {
            let _ = write!(out, r#" {k}="{}""#, trim(v.clamp(0.0, 1.0), 3));
        }
    }
    let _ = write!(out, r#" presentation:node-type="{node}" presentation:preset-class="{class}" presentation:preset-id="{}""#, esc(&preset));
    if let Some(b) = build {
        let _ = write!(out, r#" presentation:group-id="{b}""#);
    }
    if !sub.is_empty() {
        let _ = write!(out, r#" presentation:preset-sub-type="{}""#, esc(&sub));
    }
    let _ = write!(out, ">{}</anim:par>", nodes(a, t, ctx));
}

#[cfg(test)]
mod tests {
    use super::*;
    use deckcraft_model::anim::TRANSITIONS as KINDS;

    /// Regression: a by-paragraph build past the page's effect budget kept its first paragraphs
    /// only; it plays as one block instead.
    #[test]
    fn a_build_past_the_budget_plays_whole() {
        use deckcraft_model::text::{Paragraph, Run, TextBody};
        let paragraphs = (0..MAX_EFFECTS + 5).map(|i| Paragraph { runs: vec![Run::new(format!("p{i}"))], ..Default::default() }).collect();
        let shape = deckcraft_model::Shape { text: Some(TextBody { paragraphs, ..Default::default() }), ..Default::default() };
        let a = Animation { shape: shape.id, text_build: TextBuild::ByParagraph, ..Default::default() };
        let out = expand_builds(&[a], std::slice::from_ref(&shape));
        assert_eq!(out.len(), 1);
        assert_eq!((out[0].a.text_build, out[0].a.paragraph, out[0].build), (TextBuild::AsOne, None, None));
        // Regression: the budget was checked in effects but spent in paragraphs, so blank ones
        // (no effect of their own) cut the build short.
        let texts = ["a", "b", "", "", "c"];
        let paragraphs = texts.iter().map(|t| Paragraph { runs: vec![Run::new(*t)], ..Default::default() }).collect();
        let shape = deckcraft_model::Shape { text: Some(TextBody { paragraphs, ..Default::default() }), ..Default::default() };
        let mut anims = vec![Animation::default(); MAX_EFFECTS - 4];
        anims.push(Animation { shape: shape.id, text_build: TextBuild::ByParagraph, ..Default::default() });
        let out = expand_builds(&anims, std::slice::from_ref(&shape));
        let built: Vec<Option<u32>> = out.iter().filter(|e| e.build.is_some()).map(|e| e.a.paragraph).collect();
        assert_eq!(built, [Some(0), Some(1), Some(4)]);
    }

    #[test]
    fn clocks_and_durations() {
        assert_eq!(clock("1.5s"), Some(1500));
        assert_eq!(clock("250ms"), Some(250));
        assert_eq!(clock("00:00:02"), Some(2000));
        assert_eq!(clock("0.5min"), Some(30_000));
        assert_eq!(clock("0.01h"), Some(36_000));
        assert_eq!(duration_iso("PT00H00M05S"), Some(5000));
        assert_eq!(duration_iso("PT1.5S"), Some(1500));
        assert_eq!(duration_iso("bogus"), None);
    }

    /// A text group joins into one effect only as the build or block it is: each on a click is
    /// a by-paragraph build, all together one block, anything else stays as its effects.
    #[test]
    fn text_groups_join_only_as_they_play() {
        let e = |start, p: u32| (Animation { start, paragraph: Some(p), ..Default::default() }, Some("g"));
        let joined = |starts: [AnimStart; 3]| join_builds(starts.iter().enumerate().map(|(i, s)| e(*s, i as u32)).collect());
        let built = joined([AnimStart::OnClick; 3]);
        assert_eq!((built.len(), built[0].text_build, built[0].paragraph), (1, TextBuild::ByParagraph, None));
        let block = joined([AnimStart::OnClick, AnimStart::WithPrevious, AnimStart::WithPrevious]);
        assert_eq!((block.len(), block[0].text_build, block[0].paragraph), (1, TextBuild::AsOne, None));
        assert_eq!(joined([AnimStart::OnClick, AnimStart::AfterPrevious, AnimStart::AfterPrevious]).len(), 3);
        // A lone paragraph effect stays on its paragraph.
        assert_eq!(join_builds(vec![e(AnimStart::OnClick, 1)])[0].paragraph, Some(1));
    }

    #[test]
    fn presets_map_both_ways() {
        for (class, preset, sub, effect, opt) in [
            (AnimClass::Entrance, "ooo-entrance-fly-in", "from-left", "fly", "l"),
            (AnimClass::Entrance, "ooo-entrance-fade-in", "", "fade", ""),
            (AnimClass::Exit, "ooo-exit-wipe", "from-top", "wipeOut", "t"),
            (AnimClass::Entrance, "ooo-entrance-plus", "in", "shape", "plusIn"),
            (AnimClass::Exit, "ooo-exit-circle", "out", "shapeOut", "circleOut"),
            (AnimClass::Entrance, "ooo-entrance-diagonal-squares", "left-to-top", "strips", "lu"),
        ] {
            let (e, o) = effect_of(class, preset, sub);
            assert_eq!((e.as_str(), o.as_str()), (effect, opt));
            let a = Animation { class, effect: e, option: o, ..Default::default() };
            assert_eq!(preset_of(&a), (preset.to_string(), sub.to_string()));
        }
    }

    /// Every effect and option we offer is written as a preset that reads back as itself; the
    /// shape effects read back as the shape they name (`shape` `circleIn` → `circle` `in`).
    #[test]
    fn every_effect_round_trips() {
        for (id, _, class, _, _, options) in ANIMATIONS {
            for opt in std::iter::once(&"").chain(options.iter()) {
                let a = Animation { class: *class, effect: id.to_string(), option: opt.to_string(), ..Default::default() };
                let (preset, sub) = preset_of(&a);
                let (e, o) = effect_of(*class, &preset, &sub);
                let b = Animation { effect: e.clone(), option: o.clone(), ..a.clone() };
                assert_eq!(preset_of(&b), (preset.clone(), sub.clone()), "{id} {opt}: {preset} {sub}");
                let shape_alias = (*id == "shape" && !opt.starts_with("plus")) || (*id == "shapeOut" && opt.is_empty());
                let spin = *id == "spin";
                if !shape_alias && !spin {
                    assert_eq!((e.as_str(), o.as_str()), (*id, *opt), "{preset} {sub}");
                }
            }
        }
    }

    /// Every transition is written as a filter that reads back as a transition writing the same
    /// filter, and every table row reads back as itself.
    #[test]
    fn every_transition_round_trips() {
        for (kind, _, _, _, options) in KINDS {
            for opt in std::iter::once(&"").chain(options.iter()) {
                let t = Transition { kind: kind.to_string(), option: opt.to_string(), ..Default::default() };
                let Some(smil) = transition_smil(&t) else { continue };
                let (k, o) = transition_of(smil.0, smil.1, smil.2);
                let back = Transition { kind: k.into(), option: o.into(), ..Default::default() };
                assert_eq!(transition_smil(&back), Some(smil), "{kind} {opt}");
            }
        }
        for row in TRANSITIONS {
            let t = Transition { kind: row.0.into(), option: row.1.into(), ..Default::default() };
            let smil = transition_smil(&t);
            assert_eq!(smil, Some((row.2, row.3, row.4)));
            if TRANSITIONS.iter().filter(|r| (r.2, r.3, r.4) == (row.2, row.3, row.4)).count() == 1 {
                assert_eq!(transition_of(row.2, row.3, row.4), (row.0, row.1));
            }
        }
    }

    #[test]
    fn uncover_keeps_its_direction() {
        let t = Transition { kind: "uncover".into(), option: "l".into(), ..Default::default() };
        let (ty, sub, rev) = transition_smil(&t).unwrap();
        assert_eq!(transition_of(ty, sub, rev), ("uncover", "l"));
    }

    #[test]
    fn spans_saturate_and_count_repeats() {
        let a = Animation { duration_ms: 1000, delay_ms: 500, repeat: 3, auto_reverse: true, ..Default::default() };
        assert_eq!(span(&a), 6500);
        let huge = Animation { duration_ms: u32::MAX, delay_ms: u32::MAX, repeat: 1000, ..Default::default() };
        assert_eq!(span(&huge), u32::MAX);
        let forever = Animation { duration_ms: 1000, repeat: u32::MAX, ..Default::default() };
        assert_eq!(span(&forever), 1000);
    }
}
