//! Interaction tables: from the lobby naming conventions, or from a prefab dump
//! (the per-lobby object of the community `interactions.json`, game data the user supplies).

use bal2d_pack::{ClipMix, Drag, Interactions};
use bal2d_spine::{Skeleton, SkeletonData};

fn has(d: &SkeletonData, n: &str) -> bool {
    d.find_animation(n).is_some()
}

fn name_or_empty(d: &SkeletonData, n: &str) -> String {
    if has(d, n) { n.to_string() } else { String::new() }
}

fn talks(d: &SkeletonData) -> Vec<(String, String)> {
    let mut talks = Vec::new();
    for i in 1..=20 {
        let m = format!("Talk_{i:02}_M");
        let a = format!("Talk_{i:02}_A");
        if has(d, &m) || has(d, &a) {
            talks.push((name_or_empty(d, &m), name_or_empty(d, &a)));
        }
    }
    talks
}

fn idle(d: &SkeletonData) -> String {
    if has(d, "Idle_01") {
        "Idle_01".to_string()
    } else {
        d.animations.iter().find(|a| a.name.starts_with("Idle")).map(|a| a.name.clone()).unwrap_or_default()
    }
}

/// Defaults typical of the lobby prefabs: the head pat moves Touch_Point vertically inside a
/// box around the head; the eyes follow the finger anywhere on screen.
pub fn from_conventions(d: &SkeletonData, layer: u8) -> Interactions {
    let setup = |bone: &str| -> Option<(f32, f32)> {
        let b = d.find_bone(bone)?;
        Some((d.bones[b].x, d.bones[b].y))
    };
    let mut drags = Vec::new();
    if let Some((x, y)) = setup("Touch_Point") {
        drags.push(Drag {
            bone: "Touch_Point".into(),
            ing_m: name_or_empty(d, "Pat_01_M"),
            ing_a: name_or_empty(d, "Pat_01_A"),
            end_m: name_or_empty(d, "PatEnd_01_M"),
            end_a: name_or_empty(d, "PatEnd_01_A"),
            min_x: x,
            min_y: y - 30.0,
            max_x: x,
            max_y: y + 35.0,
            follow: 0.21,
            release: 0.15,
            delay: 0.1,
            box_w: 500.0,
            box_h: 280.0,
        });
    }
    if let Some((x, y)) = setup("Touch_Eye") {
        drags.push(Drag {
            bone: "Touch_Eye".into(),
            ing_m: name_or_empty(d, "Look_01_M"),
            ing_a: name_or_empty(d, "Look_01_A"),
            end_m: name_or_empty(d, "LookEnd_01_M"),
            end_a: name_or_empty(d, "LookEnd_01_A"),
            min_x: x - 60.0,
            min_y: y - 50.0,
            max_x: x + 60.0,
            max_y: y + 60.0,
            follow: 0.16,
            release: 0.07,
            delay: 0.1,
            box_w: 0.0,
            box_h: 0.0,
        });
    }
    let _ = Skeleton::new;
    Interactions {
        layer,
        default_mix: 0.25,
        intro: name_or_empty(d, "Start_Idle_01"),
        idle: idle(d),
        talks: talks(d),
        drags,
        mixes: Vec::new(),
    }
}

/// Minimal JSON reader for the prefab dump (no serde in the device-shared crates).
pub mod json {
    #[derive(Clone, Debug, PartialEq)]
    pub enum V {
        Null,
        Bool(bool),
        Num(f64),
        Str(String),
        Arr(Vec<V>),
        Obj(Vec<(String, V)>),
    }

    impl V {
        pub fn get(&self, k: &str) -> Option<&V> {
            match self {
                V::Obj(o) => o.iter().find(|(n, _)| n == k).map(|(_, v)| v),
                _ => None,
            }
        }
        pub fn num(&self) -> Option<f64> {
            if let V::Num(n) = self { Some(*n) } else { None }
        }
        pub fn str(&self) -> Option<&str> {
            if let V::Str(s) = self { Some(s) } else { None }
        }
        pub fn arr(&self) -> &[V] {
            if let V::Arr(a) = self { a } else { &[] }
        }
        pub fn obj(&self) -> &[(String, V)] {
            if let V::Obj(o) = self { o } else { &[] }
        }
    }

    pub fn parse(s: &str) -> Option<V> {
        let b = s.as_bytes();
        let mut i = 0;
        let v = value(b, &mut i)?;
        Some(v)
    }

    fn ws(b: &[u8], i: &mut usize) {
        while *i < b.len() && b[*i].is_ascii_whitespace() {
            *i += 1;
        }
    }

    fn value(b: &[u8], i: &mut usize) -> Option<V> {
        ws(b, i);
        match *b.get(*i)? {
            b'{' => {
                *i += 1;
                let mut o = Vec::new();
                loop {
                    ws(b, i);
                    if b.get(*i) == Some(&b'}') {
                        *i += 1;
                        return Some(V::Obj(o));
                    }
                    let V::Str(k) = value(b, i)? else { return None };
                    ws(b, i);
                    if b.get(*i) != Some(&b':') {
                        return None;
                    }
                    *i += 1;
                    let v = value(b, i)?;
                    o.push((k, v));
                    ws(b, i);
                    match b.get(*i)? {
                        b',' => *i += 1,
                        b'}' => {
                            *i += 1;
                            return Some(V::Obj(o));
                        }
                        _ => return None,
                    }
                }
            }
            b'[' => {
                *i += 1;
                let mut a = Vec::new();
                loop {
                    ws(b, i);
                    if b.get(*i) == Some(&b']') {
                        *i += 1;
                        return Some(V::Arr(a));
                    }
                    a.push(value(b, i)?);
                    ws(b, i);
                    match b.get(*i)? {
                        b',' => *i += 1,
                        b']' => {
                            *i += 1;
                            return Some(V::Arr(a));
                        }
                        _ => return None,
                    }
                }
            }
            b'"' => {
                *i += 1;
                let mut s = String::new();
                while *i < b.len() {
                    match b[*i] {
                        b'"' => {
                            *i += 1;
                            return Some(V::Str(s));
                        }
                        b'\\' => {
                            *i += 1;
                            match *b.get(*i)? {
                                b'n' => s.push('\n'),
                                b't' => s.push('\t'),
                                b'u' => {
                                    let h = std::str::from_utf8(b.get(*i + 1..*i + 5)?).ok()?;
                                    let c = u32::from_str_radix(h, 16).ok()?;
                                    s.push(char::from_u32(c).unwrap_or('?'));
                                    *i += 4;
                                }
                                c => s.push(c as char),
                            }
                            *i += 1;
                        }
                        _ => {
                            let start = *i;
                            while *i < b.len() && b[*i] != b'"' && b[*i] != b'\\' {
                                *i += 1;
                            }
                            s.push_str(std::str::from_utf8(&b[start..*i]).ok()?);
                        }
                    }
                }
                None
            }
            b't' => {
                *i += 4;
                Some(V::Bool(true))
            }
            b'f' => {
                *i += 5;
                Some(V::Bool(false))
            }
            b'n' => {
                *i += 4;
                Some(V::Null)
            }
            _ => {
                let start = *i;
                while *i < b.len() && (b[*i].is_ascii_digit() || matches!(b[*i], b'-' | b'+' | b'.' | b'e' | b'E')) {
                    *i += 1;
                }
                std::str::from_utf8(&b[start..*i]).ok()?.parse().ok().map(V::Num)
            }
        }
    }
}

/// From one lobby object of a prefab dump (`drags`, `clips`, `skeletonData.defaultMix`).
/// Unity positions are skeleton units / 100.
pub fn from_prefab(d: &SkeletonData, layer: u8, v: &json::V) -> Interactions {
    let mut out = from_conventions(d, layer);
    if let Some(m) = v.get("skeletonData").and_then(|s| s.get("defaultMix")).and_then(|x| x.num()) {
        out.default_mix = m as f32;
    }
    // Clip mixes and the sync ("A") partner of each "M" clip.
    let clips = v.get("clips").map(|c| c.obj()).unwrap_or(&[]);
    for (_, c) in clips {
        let Some(name) = c.get("ClipName").and_then(|x| x.str()) else { continue };
        let dflt_in = c.get("UseDefaultIntroMix").and_then(|x| x.num()).unwrap_or(1.0) != 0.0;
        let dflt_out = c.get("UseDefaultOutroMix").and_then(|x| x.num()).unwrap_or(1.0) != 0.0;
        let intro = if dflt_in { -1.0 } else { c.get("IntroMix").and_then(|x| x.num()).unwrap_or(-1.0) as f32 };
        let outro = if dflt_out { -1.0 } else { c.get("OutroMix").and_then(|x| x.num()).unwrap_or(-1.0) as f32 };
        if intro >= 0.0 || outro >= 0.0 {
            out.mixes.push(ClipMix { name: name.to_string(), intro, outro });
        }
    }
    let partner = |m: &str| -> String {
        let a = m.strip_suffix("_M").map(|b| format!("{b}_A")).unwrap_or_default();
        name_or_empty(d, &a)
    };
    let mut drags = Vec::new();
    for dr in v.get("drags").map(|x| x.arr()).unwrap_or(&[]) {
        let Some(bone) = dr.get("boneName").and_then(|x| x.str()) else { continue };
        if d.find_bone(bone).is_none() {
            continue;
        }
        let clip = |k: &str| dr.get(k).and_then(|c| c.get("clip")).and_then(|x| x.str()).map(|s| name_or_empty(d, s)).unwrap_or_default();
        let p = |k: &str, a: &str| dr.get(k).and_then(|o| o.get(a)).and_then(|x| x.num()).unwrap_or(0.0) as f32 * 100.0;
        let size = dr.get("colliders").and_then(|c| c.arr().first()).and_then(|c| c.get("m_Size"));
        let (bw, bh) = match size {
            Some(s) => (s.get("x").and_then(|x| x.num()).unwrap_or(0.0) as f32, s.get("y").and_then(|x| x.num()).unwrap_or(0.0) as f32),
            None => (0.0, 0.0),
        };
        // A collider as large as the game frame means "anywhere".
        let (bw, bh) = if bw >= 2960.0 { (0.0, 0.0) } else { (bw, bh) };
        let ing_m = clip("IngClip");
        let end_m = clip("EndClip");
        drags.push(Drag {
            bone: bone.to_string(),
            ing_a: partner(&ing_m),
            end_a: partner(&end_m),
            ing_m,
            end_m,
            min_x: p("MinLocalPos", "x"),
            min_y: p("MinLocalPos", "y"),
            max_x: p("MaxLocalPos", "x"),
            max_y: p("MaxLocalPos", "y"),
            follow: dr.get("FollowDragSpeed01").and_then(|x| x.num()).unwrap_or(0.16) as f32,
            release: dr.get("FollowReleaseSpeed01").and_then(|x| x.num()).unwrap_or(0.07) as f32,
            delay: dr.get("TriggerDelay").and_then(|x| x.num()).unwrap_or(0.1) as f32,
            box_w: bw,
            box_h: bh,
        });
    }
    if !drags.is_empty() {
        // Small boxes first; the full-screen eye follow last.
        drags.sort_by(|a, b| (a.box_w == 0.0).cmp(&(b.box_w == 0.0)).then((a.box_w * a.box_h).total_cmp(&(b.box_w * b.box_h))));
        out.drags = drags;
    }
    out
}
