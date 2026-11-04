//! The online character library's data, parsed from third-party community files (no network
//! here, so all of it runs in host tests):
//!
//! - the lobby list, `memorial/Characters.json` of asdfdsa12/BA-Spine-Viewer (ids like
//!   `Hoshino_home`, Korean names);
//! - the game's `MemoryLobbyExcelTable` (electricgoat/ba-data), which maps a lobby to its student
//!   (`PrefabName` `"Lobby" + X` for lobby `X_home`);
//! - SchaleDB's `students.min.json` for the Chinese (CN server) names;
//! - the game's `CharacterDialogExcelTable`: the memorial-lobby lines (`UILobbySpecial`), one
//!   talk per (CharacterId, GroupId), its first row naming the talk animation.

use std::collections::HashMap;
use std::io::Read;

use serde::Deserialize;
use serde::de::{self, SeqAccess, Visitor};

/// One lobby of the online library.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LibEntry {
    /// The lobby's skeleton name, e.g. `Hoshino_home`.
    pub id: String,
    /// The Chinese name, else the Korean one.
    pub name: String,
    pub kr: String,
    pub new: bool,
    /// The student (`CharacterId`), when the game tables know the lobby.
    pub character: Option<u32>,
    /// Background skeletons that belong to the lobby (`X_home_background`, `X_bg`).
    pub extras: Vec<String>,
}

/// The bubble text language.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    Hans,
    Hant,
    Ja,
    En,
}

impl Lang {
    /// The UI setting's value; `None` for no dialogue.
    pub fn from_setting(value: &str) -> Option<Lang> {
        match value {
            "off" => None,
            "zh-hant" => Some(Lang::Hant),
            "ja" => Some(Lang::Ja),
            "en" => Some(Lang::En),
            _ => Some(Lang::Hans),
        }
    }
}

/// One line of a talk with its texts (empty when the table has none).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TalkLine {
    pub ms: u32,
    pub thought: bool,
    pub tw: String,
    pub jp: String,
    pub en: String,
}

/// One talk: the animation key (`Talk_01`) and its lines in order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Talk {
    pub key: String,
    pub lines: Vec<TalkLine>,
}

/// CharacterId -> its talks, ordered as the table orders them.
pub type DialogMap = HashMap<u32, Vec<Talk>>;

fn json_err(what: &str, e: serde_json::Error) -> String {
    format!("{what}格式不对（{e}）")
}

// -- catalog -------------------------------------------------------------------------------------

#[derive(Deserialize)]
struct CharactersFile {
    characters: Vec<CharItem>,
}

#[derive(Deserialize)]
struct CharItem {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    new: Option<bool>,
}

/// A catalog entry: (id, Korean name, new).
pub type CatalogItem = (String, String, bool);

pub fn parse_characters(json: impl Read) -> Result<Vec<CatalogItem>, String> {
    let f: CharactersFile = serde_json::from_reader(json).map_err(|e| json_err("角色列表", e))?;
    Ok(f.characters.into_iter().map(|c| (c.id, c.name.unwrap_or_default(), c.new.unwrap_or(false))).collect())
}

#[derive(Deserialize)]
struct MemoryLobbyFile {
    #[serde(rename = "DataList")]
    rows: Vec<MemoryLobbyRow>,
}

#[derive(Deserialize)]
struct MemoryLobbyRow {
    #[serde(rename = "CharacterId", default)]
    character: Option<i64>,
    #[serde(rename = "PrefabName", default)]
    prefab: Option<String>,
}

/// Adds a `MemoryLobbyExcelTable`'s prefabs (lowercased, e.g. `lobbyhoshino`) to `map`; names
/// already there are kept.
pub fn parse_prefabs(json: impl Read, map: &mut HashMap<String, u32>) -> Result<(), String> {
    let f: MemoryLobbyFile = serde_json::from_reader(json).map_err(|e| json_err("大厅对照表", e))?;
    for r in f.rows {
        if let (Some(c), Some(p)) = (r.character.and_then(|c| u32::try_from(c).ok()), r.prefab) {
            map.entry(p.to_ascii_lowercase()).or_insert(c);
        }
    }
    Ok(())
}

#[derive(Deserialize)]
struct Student {
    #[serde(rename = "Id")]
    id: u32,
    #[serde(rename = "Name", default)]
    name: Option<String>,
}

/// SchaleDB's students (an object keyed by id, or a list in older copies): id -> name.
pub fn parse_students(json: &[u8]) -> Result<HashMap<u32, String>, String> {
    let first = json.iter().copied().find(|b| !b.is_ascii_whitespace() && *b != 0xEF && *b != 0xBB && *b != 0xBF);
    let list: Vec<Student> = if first == Some(b'[') {
        serde_json::from_slice(json).map_err(|e| json_err("学生名单", e))?
    } else {
        let map: HashMap<String, Student> = serde_json::from_slice(json).map_err(|e| json_err("学生名单", e))?;
        map.into_values().collect()
    };
    Ok(list.into_iter().filter_map(|s| Some((s.id, s.name.filter(|n| !n.is_empty())?))).collect())
}

/// `X_home` -> `lobbyx`, the lowercased prefab name the game tables use.
pub fn prefab_key(lobby: &str) -> String {
    let l = lobby.to_ascii_lowercase();
    format!("lobby{}", l.strip_suffix("_home").unwrap_or(&l))
}

/// The library: the catalog's lobbies (`*_home`) with their student, Chinese name and
/// background skeletons, in catalog order. Other entries (scenes, raid bosses, `Lobby*_multi`)
/// are left out.
pub fn join(items: &[CatalogItem], prefabs: &HashMap<String, u32>, names: &HashMap<u32, String>) -> Vec<LibEntry> {
    let ids: HashMap<String, &str> = items.iter().map(|(id, _, _)| (id.to_ascii_lowercase(), id.as_str())).collect();
    items
        .iter()
        .filter(|(id, _, _)| id.ends_with("_home"))
        .map(|(id, kr, new)| {
            let base = id[..id.len() - "_home".len()].to_ascii_lowercase();
            let extras = [format!("{base}_home_background"), format!("{base}_bg")]
                .iter()
                .filter_map(|k| ids.get(k).map(|s| s.to_string()))
                .collect();
            let character = prefabs.get(&prefab_key(id)).copied();
            let name = character.and_then(|c| names.get(&c)).cloned().unwrap_or_else(|| if kr.is_empty() { id.clone() } else { kr.clone() });
            LibEntry { id: id.clone(), name, kr: kr.clone(), new: *new, character, extras }
        })
        .collect()
}

/// Whether the search text (any case, Traditional characters too) is in the lobby's Chinese
/// name, Korean name or id.
pub fn matches(e: &LibEntry, query: &str) -> bool {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return true;
    }
    let simplified = crate::hans::to_simplified(&q);
    [&e.name, &e.kr, &e.id].iter().any(|f| {
        let f = f.to_lowercase();
        f.contains(&q) || f.contains(&simplified)
    })
}

/// The student for a lobby picked by hand, from its file names (skeleton or atlas names, atlas
/// page names): `Hoshino_home`, `Hoshino_home_background`, `CH0280_home_2.png` and `yuzu_bg` all
/// lead to their lobby's prefab.
pub fn character_for(names: &[String], prefabs: &HashMap<String, u32>) -> Option<u32> {
    names.iter().find_map(|n| {
        let mut s = n.rsplit('/').next().unwrap_or(n).to_ascii_lowercase();
        if let Some(dot) = s.find('.') {
            s.truncate(dot);
        }
        let mut s = s.trim_end_matches(|c: char| c.is_ascii_digit()).trim_end_matches('_').to_string();
        for suffix in ["_background", "_bg"] {
            if let Some(t) = s.strip_suffix(suffix) {
                s = t.to_string();
            }
        }
        let base = s.strip_suffix("_home").unwrap_or(&s);
        (!base.is_empty()).then(|| prefabs.get(&format!("lobby{base}")).copied()).flatten()
    })
}

// -- dialogue ------------------------------------------------------------------------------------

#[derive(Deserialize)]
struct DialogRow {
    #[serde(rename = "CharacterId", default)]
    character: Option<i64>,
    #[serde(rename = "DisplayOrder", default)]
    order: Option<i64>,
    #[serde(rename = "DialogCategory", default)]
    category: Option<String>,
    #[serde(rename = "GroupId", default)]
    group: Option<i64>,
    #[serde(rename = "DialogType", default)]
    kind: Option<String>,
    #[serde(rename = "Duration", default)]
    duration: Option<i64>,
    #[serde(rename = "AnimationName", default)]
    animation: Option<String>,
    #[serde(rename = "LocalizeTW", default)]
    tw: Option<String>,
    #[serde(rename = "LocalizeJP", default)]
    jp: Option<String>,
    #[serde(rename = "LocalizeEN", default)]
    en: Option<String>,
    /// The global table carries a few toned-down duplicates of talks for teen mode.
    #[serde(rename = "TeenMode", default)]
    teen: Option<bool>,
}

/// The `DataList` array, keeping only memorial-lobby rows while it is read (the table is
/// 11–15 MB; only about 2500 of its rows matter).
struct LobbyRows(Vec<DialogRow>);

impl<'de> Deserialize<'de> for LobbyRows {
    fn deserialize<D: de::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = LobbyRows;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("an array of dialogue rows")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<LobbyRows, A::Error> {
                let mut out = Vec::new();
                while let Some(row) = seq.next_element::<DialogRow>()? {
                    if row.category.as_deref() == Some("UILobbySpecial") && row.teen != Some(true) {
                        out.push(row);
                    }
                }
                Ok(LobbyRows(out))
            }
        }
        d.deserialize_seq(V)
    }
}

#[derive(Deserialize)]
struct DialogFile {
    #[serde(rename = "DataList")]
    rows: LobbyRows,
}

fn clean(text: Option<String>) -> String {
    let t = text.unwrap_or_default();
    let t = if t.contains('\r') { t.replace("\r\n", "\n").replace('\r', "\n") } else { t };
    t.trim().to_string()
}

/// Reads a `CharacterDialogExcelTable` and keeps the memorial-lobby talks of every student.
///
/// Rows of one (CharacterId, GroupId) ordered by DisplayOrder form a talk; a row with an
/// `AnimationName` starts one (its key is the name without `_M`/`_A`), the rows after it are the
/// next lines. `CostumeUniqueId` is the same for all of a student's rows in the current tables
/// (checked 2026-11), so it is not looked at; should a student ever get two sets of talks with
/// the same animation, the first one (by DisplayOrder) wins.
pub fn extract_dialogue(json: impl Read) -> Result<DialogMap, String> {
    let file: DialogFile = serde_json::from_reader(std::io::BufReader::with_capacity(64 * 1024, json)).map_err(|e| json_err("对话表", e))?;
    let mut groups: HashMap<(u32, i64), Vec<DialogRow>> = HashMap::new();
    for row in file.rows.0 {
        let Some(c) = row.character.and_then(|c| u32::try_from(c).ok()) else { continue };
        groups.entry((c, row.group.unwrap_or(0))).or_default().push(row);
    }
    // (character, first DisplayOrder, talk)
    let mut talks: Vec<(u32, i64, Talk)> = Vec::new();
    for ((c, _), mut rows) in groups {
        rows.sort_by_key(|r| r.order.unwrap_or(0));
        let mut current: Option<(i64, Talk)> = None;
        for r in rows {
            let anim = r.animation.as_deref().unwrap_or("").trim();
            if !anim.is_empty() {
                if let Some((o, t)) = current.take() {
                    talks.push((c, o, t));
                }
                current = Some((r.order.unwrap_or(0), Talk { key: bal2d_pack::talk_key(anim).to_string(), lines: Vec::new() }));
            }
            // Rows before the group's first animation have nothing to belong to.
            let Some((_, talk)) = current.as_mut() else { continue };
            let ms = r.duration.unwrap_or(0).clamp(0, 600_000) as u32;
            talk.lines.push(TalkLine {
                ms: if ms == 0 { 5000 } else { ms },
                thought: r.kind.as_deref() == Some("Think"),
                tw: clean(r.tw),
                jp: clean(r.jp),
                en: clean(r.en),
            });
        }
        if let Some((o, t)) = current {
            talks.push((c, o, t));
        }
    }
    talks.sort_by_key(|(c, o, _)| (*c, *o));
    let mut map = DialogMap::new();
    for (c, _, t) in talks {
        let list = map.entry(c).or_default();
        if !list.iter().any(|x| x.key.eq_ignore_ascii_case(&t.key)) {
            list.push(t);
        }
    }
    Ok(map)
}

/// The text of a line in `lang`, falling back to Japanese (newer students only have that).
pub fn line_text(l: &TalkLine, lang: Lang) -> String {
    let own = match lang {
        Lang::Hans => (!l.tw.is_empty()).then(|| crate::hans::to_simplified(&l.tw)),
        Lang::Hant => (!l.tw.is_empty()).then(|| l.tw.clone()),
        Lang::Ja => None,
        Lang::En => (!l.en.is_empty()).then(|| l.en.clone()),
    };
    own.unwrap_or_else(|| l.jp.clone())
}

/// The pack's dialogue section for these talks in `lang` (empty lines and talks left out).
pub fn dialogue(talks: &[Talk], lang: Lang) -> bal2d_pack::Dialogue {
    bal2d_pack::Dialogue {
        talks: talks
            .iter()
            .map(|t| bal2d_pack::TalkLines {
                talk: t.key.clone(),
                lines: t
                    .lines
                    .iter()
                    .filter_map(|l| {
                        let text = line_text(l, lang);
                        (!text.is_empty()).then_some(bal2d_pack::Line { text, ms: l.ms, thought: l.thought })
                    })
                    .collect(),
            })
            .filter(|t| !t.lines.is_empty())
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIR: &str = "/private/tmp/claude-501/-Volumes-EXT0-Canopus-Module-BAL2D/c01d7433-0cae-46f5-81e9-7dca51e4f2ce/scratchpad/basv";

    fn open(name: &str) -> Option<std::fs::File> {
        let f = std::fs::File::open(format!("{DIR}/{name}")).ok();
        if f.is_none() {
            eprintln!("skipped: {DIR}/{name} is missing");
        }
        f
    }

    fn prefabs() -> Option<HashMap<String, u32>> {
        let mut map = HashMap::new();
        parse_prefabs(open("g-memlobby.json")?, &mut map).unwrap();
        parse_prefabs(open("j-memlobby.json")?, &mut map).unwrap();
        Some(map)
    }

    #[test]
    fn the_catalog_joins_lobbies_with_students_and_backgrounds() {
        let (Some(chars), Some(prefabs)) = (open("Characters.json"), prefabs()) else { return };
        let Ok(students) = std::fs::read(format!("{DIR}/cn-students.json")) else { return };
        let items = parse_characters(chars).unwrap();
        assert!(items.len() > 300);
        let names = parse_students(&students).unwrap();
        assert!(names.len() > 250);
        assert_eq!(names.get(&10000).map(String::as_str), Some("爱露"));
        let lib = join(&items, &prefabs, &names);
        let chinese = lib.iter().filter(|e| e.character.is_some_and(|c| names.get(&c) == Some(&e.name))).count();
        eprintln!("{} lobbies, {chinese} with Chinese names", lib.len());
        assert!((270..=300).contains(&lib.len()), "{}", lib.len());
        assert!(chinese * 100 >= lib.len() * 95, "{chinese}/{}", lib.len());
        assert!(lib.iter().all(|e| e.id.ends_with("_home")));
        let hoshino = lib.iter().find(|e| e.id == "Hoshino_home").unwrap();
        assert_eq!(hoshino.character, Some(10005));
        assert_eq!(hoshino.extras, ["Hoshino_home_background"]);
        assert_eq!(lib.iter().find(|e| e.id == "akari_home").unwrap().extras, ["akari_bg"]);
        assert_eq!(lib.iter().find(|e| e.id == "Yuzu_home").unwrap().extras, ["yuzu_bg"]);
        assert_eq!(lib.iter().filter(|e| !e.extras.is_empty()).count(), 3);
        assert!(matches(hoshino, "hoshino") && matches(hoshino, "星野") && matches(hoshino, " HOSHINO_home "));
        let serika = lib.iter().find(|e| e.character == Some(13008)).unwrap();
        assert!(matches(serika, &serika.kr) && !matches(serika, "星野"));
        // Unknown to the game tables: the Korean name stays.
        let bg = lib.iter().find(|e| e.id == "CH0060BG_home").unwrap();
        assert_eq!((bg.character, &bg.name), (None, &bg.kr));
        // An older list-shaped copy of the students parses too.
        let list = br#"[{"Id":10000,"Name":"x","Other":[1,{"a":2}]},{"Id":10001,"Name":""}]"#;
        assert_eq!(parse_students(list).unwrap(), HashMap::from([(10000, "x".to_string())]));
    }

    #[test]
    fn hand_picked_files_find_their_student() {
        let Some(prefabs) = prefabs() else { return };
        let names = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(character_for(&names(&["document%3A1000", "Hoshino_home_background.skel"]), &prefabs), Some(10005));
        assert_eq!(character_for(&names(&["Hoshino_home"]), &prefabs), Some(10005));
        assert_eq!(character_for(&names(&["yuzu_bg"]), &prefabs), prefabs.get("lobbyyuzu").copied());
        assert!(prefabs.contains_key("lobbyyuzu"));
        let ch0280 = prefabs.get(&prefab_key("CH0280_home")).copied();
        assert!(ch0280.is_some());
        assert_eq!(character_for(&names(&["CH0280_home_2.png"]), &prefabs), ch0280);
        assert_eq!(character_for(&names(&["document%3A1000"]), &prefabs), None);
    }

    #[test]
    fn talks_are_extracted_from_the_dialogue_tables() {
        let Some(global) = open("g-dialog.json") else { return };
        let t0 = std::time::Instant::now();
        let g = extract_dialogue(global).unwrap();
        eprintln!("global: {} students in {:?}", g.len(), t0.elapsed());
        assert!(g.len() > 250);
        // Serika: the first talk is a thought followed by a spoken line.
        let serika = &g[&13008];
        let t1 = serika.iter().find(|t| t.key == "Talk_01").unwrap();
        assert_eq!(t1.lines.len(), 2);
        assert_eq!((t1.lines[0].ms, t1.lines[0].thought), (8167, true));
        assert_eq!((t1.lines[1].ms, t1.lines[1].thought), (7000, false));
        assert!(t1.lines[0].tw.contains('\n'), "line breaks are kept");
        // Every talk has a key and lines with durations; keys are unique per student.
        let mut multi = 0;
        for talks in g.values() {
            for t in talks {
                assert!(t.key.starts_with("Talk_") && !t.key.ends_with("_M"), "{}", t.key);
                assert!(!t.lines.is_empty() && t.lines.iter().all(|l| l.ms > 0 && !l.jp.is_empty()));
                multi += (t.lines.len() > 1) as usize;
            }
            let keys: std::collections::HashSet<&str> = talks.iter().map(|t| t.key.as_str()).collect();
            assert_eq!(keys.len(), talks.len());
        }
        assert!(multi > 50, "{multi} talks with several lines");
        // Karin's teen-mode duplicates are dropped: one Talk_01 with the normal text.
        let karin = &g[&20001];
        assert_eq!(karin.iter().filter(|t| t.key == "Talk_01").count(), 1);
        assert_eq!(karin[0].lines[0].tw, "這，這真的…\n能夠當作獎勵嗎…？");

        let d = dialogue(serika, Lang::Hans);
        assert_eq!(d.talks[0].talk, "Talk_01");
        assert_eq!(d.talks[0].lines[0].text, "（只能更加努力了。\n不能坐视这座城镇消失…）");
        assert!(d.talks[0].lines[0].thought);
        assert_eq!(dialogue(serika, Lang::Hant).talks[0].lines[0].text, "（只能更加努力了。\n不能坐視這座城鎮消失…）");
        assert!(dialogue(serika, Lang::En).talks[0].lines[0].text.starts_with("(I have to keep"));
        assert!(dialogue(serika, Lang::Ja).talks[0].lines[0].text.starts_with("（もっと頑張る"));
        // The pack section takes them as they are.
        let back = bal2d_pack::Dialogue::decode(&d.encode()).unwrap();
        assert_eq!(back.talks[0].lines[1].ms, 7000);

        // Students added after the global server's tables only have Japanese lines.
        let Some(jp) = open("j-dialog.json") else { return };
        let j = extract_dialogue(jp).unwrap();
        let only: Vec<u32> = j.keys().copied().filter(|c| !g.contains_key(c)).collect();
        assert!(!only.is_empty());
        let newer = &j[&only[0]];
        assert!(newer.iter().all(|t| t.lines.iter().all(|l| l.tw.is_empty() && !l.jp.is_empty())));
        let d = dialogue(newer, Lang::Hans);
        assert_eq!(d.talks[0].lines[0].text, newer[0].lines[0].jp, "falls back to Japanese");
    }
}
