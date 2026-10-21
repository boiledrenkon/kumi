//! Renoise's variations (M7): numbered takes V1…Vn of a pattern track's lines, of a phrase, or new phrases for an instrument.
//! The model says what defines the source (its anchor lines) and gives each take's recipe (operators, or lines it writes
//! itself); Kumi reads the source, draws each take from its seed and checks it against the source and the other takes
//! (re-rolling one too close or too far), writes each as one change (a phrase, or a slot of its own in a "Variations of …"
//! section), and answers a line per take. Keep, change and drop go by take number: a change or a drop never touches the source
//! or a kept take, and leaves alone a take the producer edited since Kumi wrote it.
use super::own_bridge::follow_epoch;
use super::own_changes::OwnPlan;
use super::renoise_lines::{note_value, Lines, Op};
use crate::core::{
    contracts::{JsonObject, KernelTool, Reach, ToolResult},
    errors::RuntimeError,
    refs::RefScheme,
    variations::{roll, Bounds, Material, Purpose, Take, TakeSet, TakeState, DIRECTIONS},
};
use async_trait::async_trait;
use kumi_common::abort::Signal;
use kumi_common::js::json::stringify;
use serde_json::{json, Value};
use std::{cell::RefCell, collections::BTreeMap, rc::Rc};

/// The most takes one request makes or changes, and the sets Kumi keeps.
const MOST_TAKES: usize = 16;
const SETS_KEPT: usize = 8;
/// How many draws a take gets before Kumi gives up on it.
const TRIES: u32 = 8;
/// Pages of lines read for a source (200 lines a page: a 512-line pattern is three).
const LINE_PAGES: usize = 8;
const NAME_MOST: usize = 64;

const DESCRIPTION: &str = concat!(
    "Variations in Renoise: numbered takes V1, V2… of a source, each its own phrase or slot, never written over the source. ",
    "make: source is a pattern track's lines (patternRef, or a slot for its pattern; trackRef; first and last, as Renoise numbers lines, from 00), ",
    "a phrase (phraseRef), or an instrument (instrumentRef: new phrases from lines you write); anchors are lines every take keeps as they are ",
    "(the kick and snare that make it the producer's). Each of takes is a recipe: ops (",
    "shuffle, retrig, reverse, ghosts, thin, fill, accent, octave, transpose, swing, offsets, speed; amount 0 to 1), ",
    "lines you write yourself (as notes_write takes them, numbered as the source's lines), and/or from: \"V2\" to start from another take; ",
    "direction names it in a few words. count without takes picks distinct directions. ",
    "place: patterns (each take a slot of its own in a \"Variations of …\" section at the sequence's end, its other tracks playing the source pattern's, ",
    "so playing it plays the take in context; the default for a pattern's lines) or phrases (new phrases of the instrument; the default otherwise; ",
    "phrase sets their keyTracking, baseNote (48 is C-4), lines and lpb). Kumi draws each take, checks it differs from the source and the other takes yet ",
    "keeps the anchors and the idea (re-rolling up to 8 times; novelty subtle, clear or bold), writes each as one change, and answers a line per take. ",
    "change: names (\"V6\", or \"rest\": every take not kept) and takes (a recipe each, in order; left out, Kumi picks distinct directions) rewrite only ",
    "those takes, one change each: kept takes and the source are never touched (force: true only when the producer named a kept take), and a take the ",
    "producer edited is left as it is. keep: names; at places one take: a phrase on keys [low, high], as the instrument's program, or switchAt ",
    "{patternRef, trackRef, line, col}: a 0Zxx there plays it (how a pattern switches to a faster copy, since a phrase's LPB is fixed); a pattern take ",
    "over the source's lines, after the source's slot, or at index. drop: names (\"rest\" by default) go in one change. extract: one phrase of a pattern ",
    "track's lines (source), relative to a base note so key tracking plays it from any key (phrase.baseNote: the note that plays it as written, its ",
    "first note by default; keyTracking none for a chop). list: the set's takes. Effect commands as Renoise writes them: 0Sxx sample offset, 0Rxy ",
    "retrig, 0B00 reverse, 0Zxx phrase."
);

const SCHEMA: &str = r#"{"type":"object","additionalProperties":false,"required":["action"],"properties":{
"action":{"type":"string","enum":["make","change","keep","drop","extract","list"]},
"set":{"type":"integer","minimum":1,"description":"Which set of takes (the latest by default)"},
"source":{"type":"object","additionalProperties":false,"properties":{"patternRef":{"type":"string","description":"The pattern, or a sequence slot for its pattern"},"trackRef":{"type":"string"},"first":{"type":"integer","minimum":0},"last":{"type":"integer","minimum":0},"phraseRef":{"type":"string"},"instrumentRef":{"type":"string"}}},
"anchors":{"type":"array","maxItems":512,"items":{"type":"integer","minimum":0},"description":"Lines (as Renoise numbers them) every take keeps as they are"},
"place":{"type":"string","enum":["patterns","phrases"]},
"count":{"type":"integer","minimum":1,"maximum":16},
"takes":{"type":"array","maxItems":16,"items":{"type":"object","additionalProperties":false,"properties":{"direction":{"type":"string","maxLength":80},"from":{"type":"string","description":"Another take to start from, such as V2"},"ops":{"type":"array","maxItems":12,"items":{"type":"object","additionalProperties":false,"required":["op"],"properties":{"op":{"type":"string","enum":["shuffle","retrig","reverse","ghosts","thin","fill","accent","octave","transpose","swing","offsets","speed"]},"amount":{"type":"number","minimum":0,"maximum":1},"bar":{"type":"boolean"},"faster":{"type":"boolean"},"up":{"type":"boolean"},"volume":{"type":"string"},"semitones":{"type":"integer"},"factor":{"type":"number"}}}},"lines":{"type":"array","maxItems":512,"items":{"type":"object"},"description":"Lines you write, as notes_write takes them"}}}},
"names":{"type":"array","maxItems":32,"items":{"type":"string"},"description":"Takes: V3, or rest"},
"force":{"type":"boolean"},
"novelty":{"type":"string","enum":["subtle","clear","bold"]},
"seed":{"type":"integer","minimum":0},
"phrase":{"type":"object","additionalProperties":false,"properties":{"instrumentRef":{"type":"string","description":"The instrument a pattern's lines become phrases of (the one its notes play by default)"},"keyTracking":{"type":"string","enum":["none","transpose","offset"]},"baseNote":{"type":"integer","minimum":0,"maximum":119},"lines":{"type":"integer","minimum":1,"maximum":512},"lpb":{"type":"integer","minimum":1,"maximum":256},"name":{"type":"string","maxLength":64}}},
"at":{"type":"object","additionalProperties":false,"properties":{"keys":{"type":"array","items":{"type":"integer","minimum":0,"maximum":119},"minItems":2,"maxItems":2},"program":{"type":"boolean"},"switchAt":{"type":"object","additionalProperties":false,"required":["patternRef","trackRef","line"],"properties":{"patternRef":{"type":"string"},"trackRef":{"type":"string"},"line":{"type":"integer","minimum":0},"col":{"type":"integer","minimum":1,"maximum":8}}},"over":{"type":"boolean"},"after":{"type":"boolean"},"index":{"type":"integer","minimum":1}}}
}}"#;

/// What a set varies, and where its takes go.
#[derive(Debug, Clone)]
pub struct SetInfo {
    kind: SourceKind,
    /// "lines 00–0F of “Break” on “Drums”", for the answer.
    label: String,
    /// The pattern's or phrase's name, for the takes' names.
    short: String,
    /// What the source holds (in the takes' form: a pattern's lines bound for phrases lose their instrument column);
    /// None for an instrument's new phrases.
    material: Option<Lines>,
    placement: Placement,
}

#[derive(Debug, Clone)]
enum SourceKind {
    /// A pattern track's lines first..=last: the pattern by its place in the pool, the track by its ref at `layout`.
    Range {
        pattern: Value,
        track: String,
        layout: Option<u64>,
        first: u32,
        last: u32,
    },
    Phrase,
    Instrument,
}

#[derive(Debug, Clone)]
enum Placement {
    /// New phrases of the instrument (its place), made with these settings.
    Phrases { instrument: Value, settings: JsonObject },
    /// Slots of their own; the section the first one starts.
    Patterns { section: String },
}

type Set = TakeSet<SetInfo, Value, Lines>;

/// One take's recipe.
#[derive(Debug, Clone, Default)]
struct Recipe {
    direction: String,
    ops: Vec<Op>,
    lines: Option<Vec<Value>>,
    from: Option<usize>,
}

impl Recipe {
    fn parse(value: &Value) -> Result<Recipe, String> {
        let ops =
            value.get("ops").and_then(Value::as_array).map(|ops| ops.iter().map(Op::parse).collect::<Result<Vec<_>, _>>()).transpose()?;
        let from = match value.get("from").and_then(Value::as_str) {
            Some(name) => Some(
                name.trim().trim_start_matches(['V', 'v']).parse::<usize>().map_err(|_| format!("from: {name} isn't a take such as V2"))?,
            ),
            None => None,
        };
        let lines = value.get("lines").and_then(Value::as_array).cloned();
        let ops = ops.unwrap_or_default();
        if ops.is_empty() && lines.is_none() && from.is_none() {
            return Err("each take needs ops, lines or from".into());
        }
        let direction = value.get("direction").and_then(Value::as_str).unwrap_or("").trim().to_string();
        Ok(Recipe { direction, ops, lines, from })
    }

    /// A recipe Kumi picks for a direction the model left to it.
    fn of_direction(direction: &str) -> Recipe {
        let op = |value: Value| Op::parse(&value).expect("a known operator");
        let ops = match direction {
            "denser" => vec![op(json!({"op": "fill", "amount": 0.3})), op(json!({"op": "retrig", "amount": 0.2}))],
            "sparser" => vec![op(json!({"op": "thin", "amount": 0.35}))],
            "more reverses" => vec![op(json!({"op": "reverse", "amount": 0.4}))],
            "shifted accents" => vec![op(json!({"op": "accent", "amount": 0.6}))],
            "faster rolls" => vec![op(json!({"op": "retrig", "amount": 0.4}))],
            "swapped slices" => vec![op(json!({"op": "shuffle", "amount": 0.6}))],
            "moved ghosts" => vec![op(json!({"op": "ghosts", "amount": 0.8})), op(json!({"op": "fill", "amount": 0.15}))],
            _ => vec![op(json!({"op": "octave", "amount": 0.4, "up": true}))],
        };
        Recipe { direction: direction.into(), ops, lines: None, from: None }
    }
}

/// The takes a request writes, before they're written: their number, material, recipe's direction and what was done.
struct Drawn {
    number: usize,
    material: Lines,
    direction: String,
    said: Vec<String>,
    distance: Option<f64>,
}

/// The variations of one Renoise song (the sets live as long as the integration).
pub struct RenoiseVariations<S: RefScheme + 'static> {
    plan: Rc<OwnPlan<S>>,
    sets: RefCell<Vec<Set>>,
}

impl<S: RefScheme + 'static> RenoiseVariations<S> {
    pub fn new(plan: Rc<OwnPlan<S>>) -> Rc<Self> {
        Rc::new(RenoiseVariations { plan, sets: RefCell::default() })
    }

    pub fn tool(self: &Rc<Self>) -> Rc<dyn KernelTool> {
        Rc::new(VariationsTool { owner: self.clone() })
    }

    // Reading

    fn lengthen(&self, reference: &str) -> Result<String, String> {
        let link = self.plan.link();
        let long = link.book.borrow().lengthen(&json!({"ref": reference}));
        let long = long["ref"].as_str().unwrap_or(reference).to_string();
        if super::own_bridge::SHORT_REF.is_match(&long) {
            return Err(format!("{reference} isn't one of this turn's refs (the song may have changed): read again for a current one"));
        }
        Ok(long)
    }

    fn short(&self, long: &str) -> String {
        self.plan.link().book.borrow_mut().short_ref(long)
    }

    /// A read through the tool, from the book's epoch (and `layout`, when the refs in it are from then): its answer.
    async fn read(&self, mut args: Value, layout: Option<u64>, signal: &Signal) -> Result<Value, String> {
        let link = self.plan.link();
        let input = args.as_object().cloned().unwrap_or_default();
        self.plan.place_request(&mut args, &input);
        if let Some(layout) = layout {
            args["layout"] = layout.into();
        }
        match link.call("read", args, signal.clone()).await {
            Ok(Ok(answer)) => {
                follow_epoch(&link.book, &link.epoch, &link.layout, &answer);
                Ok(answer)
            }
            Ok(Err(refused)) => Err(refused.why),
            Err(error) => Err(error.to_string()),
        }
    }

    async fn rows(&self, args: Value, signal: &Signal) -> Result<Vec<Value>, String> {
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..LINE_PAGES {
            let mut asked = args.clone();
            if let Some(cursor) = &cursor {
                asked["cursor"] = cursor.clone().into();
            }
            let page = self.read(asked, None, signal).await?;
            out.extend(page.get("items").and_then(Value::as_array).cloned().unwrap_or_default());
            match page.get("nextCursor").and_then(Value::as_str) {
                Some(next) => cursor = Some(next.into()),
                None => return Ok(out),
            }
        }
        Ok(out)
    }

    /// Where each item is now (`read {kind: "where"}`): {pattern}, {instrument}, {phrase} (a place or a ref) or {track} (a ref
    /// from `layout`).
    async fn locate(&self, items: Vec<Value>, layout: Option<u64>, signal: &Signal) -> Result<Vec<Value>, String> {
        let answer = self.read(json!({"kind": "where", "items": items}), layout, signal).await?;
        Ok(answer.get("items").and_then(Value::as_array).cloned().unwrap_or_default())
    }

    async fn song_lpb(&self, signal: &Signal) -> u32 {
        let song = self.read(json!({"kind": "song"}), None, signal).await.ok();
        song.and_then(|song| song.pointer("/items/0/lpb").and_then(Value::as_u64)).unwrap_or(4) as u32
    }

    /// A take's lines as they are now: its phrase's, or its pattern's on the source's track.
    async fn take_lines(&self, info: &SetInfo, now: &Value, track: Option<&str>, signal: &Signal) -> Result<Lines, String> {
        let reference = now.get("ref").and_then(Value::as_str).ok_or("the take wasn't found")?;
        match (&info.kind, track) {
            (SourceKind::Range { first, last, .. }, Some(track)) => {
                let rows =
                    self.rows(json!({"kind": "line", "parent": reference, "track": track, "hidden": true, "limit": 200}), signal).await?;
                let lpb = info.material.as_ref().map_or(4, |material| material.lpb);
                Ok(Lines::from_rows(&rows, *first, *last, lpb))
            }
            _ => {
                let rows = self.rows(json!({"kind": "line", "parent": reference, "hidden": true, "limit": 200}), signal).await?;
                let lines = now.get("lines").and_then(Value::as_u64).unwrap_or(16) as u32;
                let lpb = now.get("lpb").and_then(Value::as_u64).unwrap_or(4) as u32;
                Ok(Lines::from_rows(&rows, 0, lines.max(1) - 1, lpb))
            }
        }
    }

    /// The source's track now (a Range source): its ref, from the layout it was read at.
    async fn source_track(&self, info: &SetInfo, signal: &Signal) -> Result<Option<String>, String> {
        let SourceKind::Range { track, layout, .. } = &info.kind else { return Ok(None) };
        let found = self.locate(vec![json!({"track": track})], *layout, signal).await?;
        match found.first().and_then(|row| row.get("ref")).and_then(Value::as_str) {
            Some(reference) => Ok(Some(reference.to_string())),
            None => Err("the source's track isn't in the song any more".into()),
        }
    }

    // Changes

    /// One change of Kumi's by its step's name: its change id and what the DAW answered, or why it wasn't made (and whether
    /// the request must stop there).
    async fn change(&self, tool: &str, input: Value, signal: &Signal) -> Result<(String, Value), (String, bool)> {
        *self.plan.last_applied.borrow_mut() = None;
        let outcome = self.plan.change_by(tool, input.as_object().cloned().unwrap_or_default(), signal.clone()).await;
        let answer: Value = serde_json::from_str(&outcome.text).unwrap_or(Value::Null);
        if outcome.is_error || outcome.stops {
            let why = answer.get("changed").and_then(Value::as_str).map(str::to_owned).unwrap_or(outcome.text);
            return Err((why, outcome.stops));
        }
        let id = answer.get("change").and_then(Value::as_str).unwrap_or("").to_string();
        Ok((id, self.plan.last_applied.borrow_mut().take().unwrap_or(Value::Null)))
    }

    // The tool

    async fn run(&self, input: JsonObject, signal: Signal) -> Result<Value, String> {
        let action = input.get("action").and_then(Value::as_str).unwrap_or("");
        match action {
            "make" => self.make(&input, &signal).await,
            "change" => self.change_takes(&input, &signal).await,
            "keep" => self.keep(&input, &signal).await,
            "drop" => self.drop_takes(&input, &signal).await,
            "extract" => self.extract(&input, &signal).await,
            "list" => {
                let sets = self.sets.borrow();
                let set = pick_set(&sets, &input)?;
                Ok(json!({"set": set.id, "source": set.source.label, "takes": set.lines()}))
            }
            _ => Err("action is make, change, keep, drop, extract or list".into()),
        }
    }

    /// Reads the source a request names (and where its takes go): the set's info.
    async fn source_of(&self, input: &JsonObject, signal: &Signal) -> Result<SetInfo, String> {
        let source = input
            .get("source")
            .and_then(Value::as_object)
            .ok_or("make needs a source: patternRef and trackRef (with first and last), phraseRef, or instrumentRef")?;
        let text = |key: &str| source.get(key).and_then(Value::as_str);
        let phrase = input.get("phrase").and_then(Value::as_object).cloned().unwrap_or_default();
        let anchors: Vec<u32> =
            input.get("anchors").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_u64).map(|n| n as u32).collect();
        let place = input.get("place").and_then(Value::as_str);
        if let (Some(pattern), Some(track)) = (text("patternRef"), text("trackRef")) {
            let (pattern, track) = (self.lengthen(pattern)?, self.lengthen(track)?);
            let found = self.locate(vec![json!({"pattern": pattern}), json!({"track": track})], None, signal).await?;
            let (pattern_row, track_row) = (&found[0], &found[1]);
            let (Some(pattern_ref), Some(track_ref)) =
                (pattern_row.get("ref").and_then(Value::as_str), track_row.get("ref").and_then(Value::as_str))
            else {
                return Err("that pattern or track isn't in the song any more: read again".into());
            };
            let length = pattern_row.get("lines").and_then(Value::as_u64).unwrap_or(64) as u32;
            let first = source.get("first").and_then(Value::as_u64).unwrap_or(0) as u32;
            let last = source.get("last").and_then(Value::as_u64).map_or(length - 1, |n| n as u32);
            if first > last || last >= length {
                return Err(format!("first and last are lines from 00 to {:02} of this pattern, first not after last", length - 1));
            }
            let lpb = self.song_lpb(signal).await;
            let rows =
                self.rows(json!({"kind": "line", "parent": pattern_ref, "track": track_ref, "hidden": true, "limit": 200}), signal).await?;
            let mut material = Lines::from_rows(&rows, first, last, lpb);
            material.anchors = anchors_in(&anchors, first, last)?;
            let pattern_name =
                named(pattern_row, &format!("Pattern {:02}", pattern_row.get("number").and_then(Value::as_u64).unwrap_or(0)));
            let track_name = named(track_row, "the track");
            let label = format!("lines {first:02}–{last:02} of “{pattern_name}” on “{track_name}”");
            let kind = SourceKind::Range {
                pattern: pattern_row.get("place").cloned().unwrap_or(Value::Null),
                track: track_ref.to_string(),
                layout: self.plan.link().layout.get(),
                first,
                last,
            };
            let placement = match place.unwrap_or("patterns") {
                "patterns" => Placement::Patterns { section: format!("Variations of {pattern_name}") },
                _ => {
                    // (a pattern's lines become phrases of the instrument they play: its notes lose their instrument column)
                    let instrument = match phrase.get("instrumentRef").and_then(Value::as_str) {
                        Some(reference) => self.lengthen(reference)?,
                        None => {
                            let value =
                                most_played(&material).ok_or("those lines play no instrument: name one with phrase.instrumentRef")?;
                            let epoch = pattern_ref.split(':').next().unwrap_or("");
                            format!("{epoch}:instrument:{}", value as u32 + 1)
                        }
                    };
                    let found = self.locate(vec![json!({"instrument": instrument})], None, signal).await?;
                    let row = found.first().filter(|row| row.get("ref").is_some()).ok_or("that instrument isn't in the song any more")?;
                    let index = row.get("index").and_then(Value::as_u64).unwrap_or(1) as u8 - 1;
                    material = for_phrase(&material, index)?;
                    let base = phrase.get("baseNote").cloned().or_else(|| first_note(&material).map(Value::from));
                    let mut settings = phrase_settings(&phrase, material.length, lpb);
                    settings.entry("keyTracking").or_insert_with(|| json!("transpose"));
                    if let Some(base) = base {
                        settings.entry("baseNote").or_insert(base);
                    }
                    Placement::Phrases { instrument: row.get("place").cloned().unwrap_or(Value::Null), settings }
                }
            };
            return Ok(SetInfo { kind, label, short: pattern_name, material: Some(material), placement });
        }
        if place == Some("patterns") {
            return Err("place patterns is for a pattern track's lines (patternRef and trackRef)".into());
        }
        if let Some(phrase_ref) = text("phraseRef") {
            let long = self.lengthen(phrase_ref)?;
            let instrument = instrument_of(&long).ok_or("phraseRef must be a phrase")?;
            let found = self.locate(vec![json!({"phrase": long}), json!({"instrument": instrument})], None, signal).await?;
            let (row, instrument_row) = (&found[0], &found[1]);
            let reference = row.get("ref").and_then(Value::as_str).ok_or("that phrase isn't in the song any more: read again")?;
            let lines = row.get("lines").and_then(Value::as_u64).unwrap_or(16) as u32;
            let lpb = row.get("lpb").and_then(Value::as_u64).unwrap_or(4) as u32;
            let rows = self.rows(json!({"kind": "line", "parent": reference, "hidden": true, "limit": 200}), signal).await?;
            let mut material = Lines::from_rows(&rows, 0, lines.max(1) - 1, lpb);
            material.anchors = anchors_in(&anchors, 0, lines.max(1) - 1)?;
            let settings = self.phrase_row_settings(&instrument, reference, &phrase, lines, lpb, signal).await;
            let name = named(row, &format!("Phrase {:02}", row.get("index").and_then(Value::as_u64).unwrap_or(1)));
            let label = format!("phrase “{name}” of “{}”", named(instrument_row, "the instrument"));
            return Ok(SetInfo {
                kind: SourceKind::Phrase,
                label,
                short: name,
                material: Some(material),
                placement: Placement::Phrases { instrument: instrument_row.get("place").cloned().unwrap_or(Value::Null), settings },
            });
        }
        if let Some(instrument_ref) = text("instrumentRef") {
            let long = self.lengthen(instrument_ref)?;
            let found = self.locate(vec![json!({"instrument": long})], None, signal).await?;
            let row =
                found.first().filter(|row| row.get("ref").is_some()).ok_or("that instrument isn't in the song any more: read again")?;
            let name = named(row, "the instrument");
            let lpb = match phrase.get("lpb").and_then(Value::as_u64) {
                Some(lpb) => lpb as u32,
                None => self.song_lpb(signal).await,
            };
            let lines = phrase.get("lines").and_then(Value::as_u64).map(|n| n as u32).unwrap_or_else(|| written_length(input, 16));
            return Ok(SetInfo {
                kind: SourceKind::Instrument,
                label: format!("new phrases of “{name}”"),
                short: name,
                material: None,
                placement: Placement::Phrases {
                    instrument: row.get("place").cloned().unwrap_or(Value::Null),
                    settings: phrase_settings(&phrase, lines, lpb),
                },
            });
        }
        Err("source is patternRef and trackRef (with first and last), phraseRef, or instrumentRef".into())
    }

    /// A phrase source's own settings (key tracking, base note), for its takes; what `phrase` gives wins.
    async fn phrase_row_settings(
        &self,
        instrument: &str,
        reference: &str,
        given: &JsonObject,
        lines: u32,
        lpb: u32,
        signal: &Signal,
    ) -> JsonObject {
        let mut settings = phrase_settings(given, lines, lpb);
        if let Ok(rows) = self.rows(json!({"kind": "phrase", "parent": instrument, "limit": 200}), signal).await {
            if let Some(row) = rows.iter().find(|row| row.get("ref").and_then(Value::as_str) == Some(reference)) {
                for key in ["keyTracking", "baseNote"] {
                    if let Some(value) = row.get(key) {
                        settings.entry(key).or_insert(value.clone());
                    }
                }
            }
        }
        settings
    }

    async fn make(&self, input: &JsonObject, signal: &Signal) -> Result<Value, String> {
        let info = self.source_of(input, signal).await?;
        let recipes = recipes_of(input, info.material.is_some())?;
        let bounds = bounds_of(input);
        let seed = input.get("seed").and_then(Value::as_u64).unwrap_or_else(fresh_seed);
        let id = self.sets.borrow().iter().map(|set| set.id).max().unwrap_or(0) + 1;
        let mut set: Set = TakeSet::new(id, info, seed);
        let mut answer_lines = Vec::new();
        let mut skipped = Vec::new();
        for recipe in recipes {
            let number = set.next_number();
            let drawn = match self.draw(&set, number, &recipe, &bounds) {
                Ok(drawn) => drawn,
                Err(why) => {
                    skipped.push(format!("V{number} ({}) wasn't made: {why}", direction_of(&recipe)));
                    continue;
                }
            };
            match self.write_new(&set, &drawn, signal).await {
                Ok((change, place, reference)) => {
                    let line = take_line(&drawn, &change);
                    answer_lines.push(json!({"take": format!("V{number}"), "ref": self.short(&reference), "line": line}));
                    set.takes.push(Take {
                        number,
                        state: TakeState::Shown,
                        place,
                        change: Some(change),
                        direction: drawn.direction.clone(),
                        digest: drawn.material.digest(),
                        material: drawn.material,
                        line,
                    });
                }
                Err((why, stops)) => {
                    skipped.push(format!("V{number} ({}) wasn't made: {why}", direction_of(&recipe)));
                    if stops {
                        break;
                    }
                }
            }
        }
        let made = set.takes.len();
        let reply = json!({"set": id, "source": set.source.label, "takes": answer_lines, "skipped": skipped});
        if made > 0 {
            let mut sets = self.sets.borrow_mut();
            sets.push(set);
            let excess = sets.len().saturating_sub(SETS_KEPT);
            sets.drain(..excess);
        }
        Ok(reply)
    }

    /// Draws take `number` by its recipe and checks it (re-rolling a drawn one): what it is and what was done.
    /// (A changed take is checked against its old self too: a change must change it.)
    fn draw(&self, set: &Set, number: usize, recipe: &Recipe, bounds: &Bounds) -> Result<Drawn, String> {
        let source = set.source.material.as_ref();
        let base: Lines = match recipe.from {
            Some(from) => set
                .take(from)
                .filter(|take| take.state != TakeState::Dropped)
                .map(|take| take.material.clone())
                .ok_or(format!("from: there is no V{from} to start from"))?,
            None => match source {
                Some(source) => source.clone(),
                None => empty_lines(set),
            },
        };
        let others_owned: Vec<(usize, Lines)> =
            set.living_except(&[]).into_iter().map(|take| (take.number, take.material.clone())).collect();
        let others: Vec<(usize, &Lines)> = others_owned.iter().map(|(n, lines)| (*n, lines)).collect();
        let offset = match &set.source.kind {
            SourceKind::Range { first, .. } => *first,
            _ => 0,
        };
        // (written lines are what they are: one try; operators draw again from another seed)
        let tries = if recipe.ops.is_empty() { 1 } else { TRIES };
        let said: RefCell<Vec<String>> = RefCell::default();
        let rolled = roll(set.seed, number, tries, source, &others, bounds, |seed| {
            let mut take = match &recipe.lines {
                Some(lines) => {
                    let mut written = Lines::from_cells(lines, offset, base.length, base.lpb)?;
                    written.anchors = base.anchors.clone();
                    written
                }
                None => base.clone(),
            };
            let mut rng = crate::core::variations::Rng::new(seed);
            let mut words = Vec::new();
            if recipe.lines.is_some() {
                words.push("lines as written".to_string());
            }
            for op in &recipe.ops {
                if let Some(done) = op.apply(&mut take, source.unwrap_or(&base), &mut rng) {
                    words.push(done);
                }
            }
            if let Some(source) = source {
                if let Err(line) = source.anchors_kept_in(&take) {
                    return Err(format!("line {:02} is an anchor: every take keeps it as it is", line + offset));
                }
            }
            *said.borrow_mut() = words;
            Ok(take)
        });
        let take = rolled.take.filter(|_| rolled.verdict.is_fine()).ok_or_else(|| {
            let tried = if rolled.tries > 1 { format!(" (after {} tries)", rolled.tries) } else { String::new() };
            format!("{}{tried}", rolled.verdict.words())
        })?;
        let distance = source.map(|source| take.distance(source));
        let mut words = said.into_inner();
        if rolled.tries > 1 {
            words.push(format!("re-rolled {} times", rolled.tries - 1));
        }
        Ok(Drawn { number, material: take, direction: direction_of(recipe).to_string(), said: words, distance })
    }

    /// Writes a new take: its change id, its place (to find it again) and its ref now.
    async fn write_new(&self, set: &Set, drawn: &Drawn, signal: &Signal) -> Result<(String, Value, String), (String, bool)> {
        let name = take_name(drawn.number, &set.source.short);
        match (&set.source.placement, &set.source.kind) {
            (Placement::Phrases { instrument, settings }, _) => {
                let found = self.locate(vec![json!({"instrument": instrument})], None, signal).await.map_err(|why| (why, true))?;
                let reference = found
                    .first()
                    .and_then(|row| row.get("ref"))
                    .and_then(Value::as_str)
                    .ok_or(("the instrument isn't in the song any more".to_string(), true))?;
                let mut input = settings.clone();
                input.insert("ref".into(), json!(reference));
                input.insert("name".into(), json!(name));
                input.insert("lpb".into(), json!(drawn.material.lpb));
                input.insert("lines".into(), json!(drawn.material.length));
                input.insert("content".into(), Value::Array(drawn.material.cells(0)));
                let (change, applied) = self.change("phrase_add", Value::Object(input), signal).await?;
                let place = applied.pointer("/created/place").cloned().unwrap_or(Value::Null);
                let made = applied.pointer("/created/ref").and_then(Value::as_str).unwrap_or("").to_string();
                Ok((change, place, made))
            }
            (Placement::Patterns { section }, SourceKind::Range { pattern, track, layout, first, last }) => {
                // (the source pattern and track where they are now, and the slot of the take before this one: the takes stay together)
                let mut items = vec![json!({"pattern": pattern}), json!({"track": track})];
                let previous = set.takes.iter().rev().find(|take| take.state != TakeState::Dropped);
                if let Some(previous) = previous {
                    items.push(json!({"pattern": previous.place}));
                }
                let found = self.locate(items, *layout, signal).await.map_err(|why| (why, true))?;
                let (Some(pattern_ref), Some(track_ref)) =
                    (found[0].get("ref").and_then(Value::as_str), found[1].get("ref").and_then(Value::as_str))
                else {
                    return Err(("the source's pattern or track isn't in the song any more".into(), true));
                };
                let mut input = json!({"patternRef": pattern_ref, "trackRef": track_ref, "first": first, "last": last,
                    "content": drawn.material.cells(*first), "name": name});
                match found
                    .get(2)
                    .and_then(|row| row.get("slots"))
                    .and_then(Value::as_array)
                    .and_then(|slots| slots.last())
                    .and_then(Value::as_u64)
                {
                    Some(slot) => input["index"] = json!(slot + 1),
                    None => input["section"] = json!(section),
                }
                let (change, applied) = self.change("slot_variation", input, signal).await?;
                let place = applied.pointer("/created/place").cloned().unwrap_or(Value::Null);
                let made = applied.pointer("/created/pattern").and_then(Value::as_str).unwrap_or("").to_string();
                Ok((change, place, made))
            }
            _ => Err(("this set's takes can't go in patterns".into(), false)),
        }
    }

    async fn change_takes(&self, input: &JsonObject, signal: &Signal) -> Result<Value, String> {
        let force = input.get("force") == Some(&Value::Bool(true));
        let names = names_of(input, &[]);
        if names.is_empty() {
            return Err("change needs names: the takes to change (V6, or rest)".into());
        }
        let (numbers, mut set) = {
            let sets = self.sets.borrow();
            let set = pick_set(&sets, input)?;
            (set.pick(&names, Purpose::Change, force)?, set.clone())
        };
        let given: Vec<Recipe> =
            input.get("takes").and_then(Value::as_array).into_iter().flatten().map(Recipe::parse).collect::<Result<_, _>>()?;
        if given.len() > numbers.len() || numbers.len() > MOST_TAKES {
            return Err(format!(
                "change names {} takes and gives {} recipes: give one per take, or fewer and Kumi picks the rest",
                numbers.len(),
                given.len()
            ));
        }
        // (the directions the model left to Kumi: distinct from the ones it gave and from each other)
        let mut used: Vec<String> = given.iter().map(|recipe| recipe.direction.clone()).collect();
        let mut recipes = given;
        while recipes.len() < numbers.len() {
            let direction = DIRECTIONS.iter().find(|d| !used.iter().any(|u| u == *d)).copied().unwrap_or("swapped slices");
            used.push(direction.into());
            let mut recipe = Recipe::of_direction(direction);
            if set.source.material.is_none() {
                recipe.from = Some(numbers[recipes.len()]);
            }
            recipes.push(recipe);
        }
        let bounds = bounds_of(input);
        let track = self.source_track(&set.source, signal).await?;
        let mut lines_out = Vec::new();
        let mut skipped = Vec::new();
        for (number, recipe) in numbers.iter().copied().zip(recipes) {
            let take = set.take(number).expect("a picked take").clone();
            let found = self.locate(vec![json!(take_item(&set.source, &take.place))], None, signal).await?;
            let now = found.into_iter().next().unwrap_or(Value::Null);
            if now.get("ref").is_none() {
                skipped.push(format!("V{number} isn't in the song any more"));
                continue;
            }
            let current = self.take_lines(&set.source, &now, track.as_deref(), signal).await?;
            if current.digest() != take.digest {
                skipped.push(format!("V{number} was edited in Renoise since Kumi wrote it: left as it is"));
                continue;
            }
            let mut recipe = recipe;
            if recipe.from.is_none() && set.source.material.is_none() {
                recipe.from = Some(number);
            }
            let drawn = match self.draw(&set, number, &recipe, &bounds) {
                Ok(drawn) => drawn,
                Err(why) => {
                    skipped.push(format!("V{number} wasn't changed: {why}"));
                    continue;
                }
            };
            let reference = now.get("ref").and_then(Value::as_str).unwrap_or("").to_string();
            let mut rows = drawn.material.clone();
            rows.anchors = current.anchors.clone();
            let written = match &set.source.kind {
                SourceKind::Range { first, .. } => {
                    let input = json!({"patternRef": reference, "trackRef": track.clone().unwrap_or_default(), "lines": drawn.material.rewrite(&current, *first)});
                    self.change("notes_write", input, signal).await
                }
                _ => {
                    let mut first = Ok((String::new(), Value::Null));
                    if drawn.material.lpb != current.lpb {
                        first = self.change("phrase_set", json!({"ref": reference, "lpb": drawn.material.lpb}), signal).await;
                    }
                    match first {
                        Ok(_) => {
                            self.change("phrase_write", json!({"ref": reference, "lines": drawn.material.rewrite(&current, 0)}), signal)
                                .await
                        }
                        Err(why) => Err(why),
                    }
                }
            };
            match written {
                Ok((change, _)) => {
                    let line = take_line(&drawn, &change);
                    lines_out.push(json!({"take": format!("V{number}"), "ref": self.short(&reference), "line": line}));
                    if let Some(take) = set.take_mut(number) {
                        take.digest = drawn.material.digest();
                        take.material = drawn.material;
                        take.direction = drawn.direction;
                        take.change = Some(change);
                        take.line = line;
                    }
                }
                Err((why, stops)) => {
                    skipped.push(format!("V{number} wasn't changed: {why}"));
                    if stops {
                        break;
                    }
                }
            }
        }
        let reply = json!({"set": set.id, "changed": lines_out, "skipped": skipped, "takes": set.lines()});
        self.put_back(set);
        Ok(reply)
    }

    async fn keep(&self, input: &JsonObject, signal: &Signal) -> Result<Value, String> {
        let names = names_of(input, &[]);
        if names.is_empty() {
            return Err("keep needs names: the takes the producer keeps (V3)".into());
        }
        let (numbers, mut set) = {
            let sets = self.sets.borrow();
            let set = pick_set(&sets, input)?;
            (set.pick(&names, Purpose::Keep, false)?, set.clone())
        };
        let mut placed = Value::Null;
        if let Some(at) = input.get("at").and_then(Value::as_object) {
            let [number] = numbers.as_slice() else { return Err("at places one take: keep the others without at".into()) };
            placed = self.place_take(&set, *number, at, signal).await?;
        }
        for number in &numbers {
            if let Some(take) = set.take_mut(*number) {
                take.state = TakeState::Kept;
            }
        }
        let rest: Vec<String> =
            set.takes.iter().filter(|take| take.state == TakeState::Shown).map(|take| format!("V{}", take.number)).collect();
        let reply = json!({"set": set.id, "kept": numbers.iter().map(|n| format!("V{n}")).collect::<Vec<_>>(), "placed": placed, "rest": rest, "takes": set.lines()});
        self.put_back(set);
        Ok(reply)
    }

    /// One change that puts a kept take where the producer says.
    async fn place_take(&self, set: &Set, number: usize, at: &JsonObject, signal: &Signal) -> Result<Value, String> {
        let take = set.take(number).expect("a picked take");
        let found = self.locate(vec![take_item(&set.source, &take.place)], None, signal).await?;
        let now = found.into_iter().next().unwrap_or(Value::Null);
        let reference = now.get("ref").and_then(Value::as_str).ok_or(format!("V{number} isn't in the song any more"))?.to_string();
        let (tool, input) = match &set.source.placement {
            Placement::Phrases { instrument, .. } => {
                if let Some(keys) = at.get("keys") {
                    ("phrase_map", json!({"ref": reference, "noteRange": keys}))
                } else if at.get("program") == Some(&Value::Bool(true)) {
                    let found = self.locate(vec![json!({"instrument": instrument})], None, signal).await?;
                    let instrument = found
                        .first()
                        .and_then(|row| row.get("ref"))
                        .and_then(Value::as_str)
                        .ok_or("the instrument isn't in the song any more")?;
                    ("phrase_playback", json!({"ref": instrument, "mode": "program", "programRef": reference}))
                } else if let Some(switch) = at.get("switchAt").and_then(Value::as_object) {
                    let pattern = self.lengthen(switch.get("patternRef").and_then(Value::as_str).unwrap_or(""))?;
                    let track = self.lengthen(switch.get("trackRef").and_then(Value::as_str).unwrap_or(""))?;
                    let line = switch.get("line").and_then(Value::as_u64).unwrap_or(0);
                    let col = switch.get("col").and_then(Value::as_u64).unwrap_or(1);
                    let index = now.get("index").and_then(Value::as_u64).unwrap_or(1);
                    (
                        "notes_write",
                        json!({"patternRef": pattern, "trackRef": track, "merge": true, "lines": [{"line": line, "effects": [{"col": col, "number": "0Z", "amount": index}]}]}),
                    )
                } else {
                    return Err("at, for a phrase take: keys, program or switchAt".into());
                }
            }
            Placement::Patterns { .. } => {
                let SourceKind::Range { pattern, track, layout, first, last } = &set.source.kind else {
                    return Err("a pattern take needs its source".into());
                };
                let found = self.locate(vec![json!({"pattern": pattern}), json!({"track": track})], *layout, signal).await?;
                let (Some(source), Some(track)) =
                    (found[0].get("ref").and_then(Value::as_str), found[1].get("ref").and_then(Value::as_str))
                else {
                    return Err("the source's pattern or track isn't in the song any more".into());
                };
                if at.get("over") == Some(&Value::Bool(true)) {
                    (
                        "lines_copy",
                        json!({"patternRef": reference, "trackRef": track, "first": first, "last": last, "toPatternRef": source, "toTrackRef": track, "line": first}),
                    )
                } else if at.get("after") == Some(&Value::Bool(true)) {
                    let slot = found[0]
                        .get("slots")
                        .and_then(Value::as_array)
                        .and_then(|slots| slots.first())
                        .and_then(Value::as_u64)
                        .ok_or("no slot plays the source pattern now")?;
                    ("slot_insert", json!({"index": slot + 1, "patternRef": reference}))
                } else if let Some(index) = at.get("index").and_then(Value::as_u64) {
                    ("slot_insert", json!({"index": index, "patternRef": reference}))
                } else {
                    return Err("at, for a pattern take: over, after or index".into());
                }
            }
        };
        let (change, applied) = self.change(tool, input, signal).await.map_err(|(why, _)| why)?;
        Ok(json!({"change": change, "title": applied.pointer("/display/title").cloned().unwrap_or(Value::Null)}))
    }

    async fn drop_takes(&self, input: &JsonObject, signal: &Signal) -> Result<Value, String> {
        let force = input.get("force") == Some(&Value::Bool(true));
        let names = names_of(input, &["rest"]);
        let (numbers, mut set) = {
            let sets = self.sets.borrow();
            let set = pick_set(&sets, input)?;
            (set.pick(&names, Purpose::Drop, force)?, set.clone())
        };
        let track = self.source_track(&set.source, signal).await?;
        let items: Vec<Value> = numbers.iter().map(|n| take_item(&set.source, &set.take(*n).expect("a picked take").place)).collect();
        let found = self.locate(items, None, signal).await?;
        let mut refs = Vec::new();
        let mut dropping = Vec::new();
        let mut skipped = Vec::new();
        for (number, now) in numbers.iter().copied().zip(found) {
            if now.get("ref").is_none() {
                skipped.push(format!("V{number} isn't in the song any more"));
                if let Some(take) = set.take_mut(number) {
                    take.state = TakeState::Dropped;
                }
                continue;
            }
            let current = self.take_lines(&set.source, &now, track.as_deref(), signal).await?;
            if current.digest() != set.take(number).expect("a picked take").digest {
                skipped.push(format!("V{number} was edited in Renoise since Kumi wrote it: left as it is"));
                continue;
            }
            match &set.source.placement {
                Placement::Phrases { .. } => refs.push(now["ref"].clone()),
                Placement::Patterns { .. } => {
                    let slots = now.get("slots").and_then(Value::as_array).cloned().unwrap_or_default();
                    if slots.len() != 1 {
                        skipped.push(format!("V{number} plays in {} slots now: left as it is", slots.len()));
                        continue;
                    }
                    refs.push(now["seqRef"].clone());
                }
            }
            dropping.push(number);
        }
        let mut change = Value::Null;
        if !refs.is_empty() {
            let (tool, field) = match set.source.placement {
                Placement::Phrases { .. } => ("phrases_drop", "phraseRefs"),
                Placement::Patterns { .. } => ("slots_drop", "slotRefs"),
            };
            let (id, applied) = self.change(tool, json!({field: refs}), signal).await.map_err(|(why, _)| why)?;
            for number in &dropping {
                if let Some(take) = set.take_mut(*number) {
                    take.state = TakeState::Dropped;
                }
            }
            change = json!({"change": id, "title": applied.pointer("/display/title").cloned().unwrap_or(Value::Null), "note": applied.pointer("/display/note").cloned().unwrap_or(Value::Null)});
        }
        let reply = json!({"set": set.id, "dropped": dropping.iter().map(|n| format!("V{n}")).collect::<Vec<_>>(), "in": change, "skipped": skipped, "takes": set.lines()});
        self.put_back(set);
        Ok(reply)
    }

    async fn extract(&self, input: &JsonObject, signal: &Signal) -> Result<Value, String> {
        let source = input.get("source").and_then(Value::as_object);
        if source.and_then(|source| source.get("patternRef")).is_none() {
            return Err("extract takes a pattern track's lines: source with patternRef, trackRef, first and last".into());
        }
        let mut asked = input.clone();
        asked.insert("place".into(), json!("phrases"));
        let info = self.source_of(&asked, signal).await?;
        let Placement::Phrases { instrument, settings } = &info.placement else { return Err("extract makes a phrase".into()) };
        let material = info.material.clone().unwrap_or_else(|| empty_lines_of(1, 4));
        if material.silent() {
            return Err("those lines have no notes to make a phrase of".into());
        }
        let found = self.locate(vec![json!({"instrument": instrument})], None, signal).await?;
        let reference =
            found.first().and_then(|row| row.get("ref")).and_then(Value::as_str).ok_or("the instrument isn't in the song any more")?;
        let mut phrase = settings.clone();
        phrase.insert("ref".into(), json!(reference));
        let name = input
            .get("phrase")
            .and_then(|phrase| phrase.get("name"))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| info.short.clone());
        phrase.insert("name".into(), json!(clip(&name, NAME_MOST)));
        phrase.insert("content".into(), Value::Array(material.cells(0)));
        let (change, applied) = self.change("phrase_add", Value::Object(phrase), signal).await.map_err(|(why, _)| why)?;
        let made = applied.pointer("/created/ref").and_then(Value::as_str).map(|r| self.short(r));
        Ok(json!({"change": change, "ref": made, "title": applied.pointer("/display/title").cloned().unwrap_or(Value::Null),
            "keyTracking": settings.get("keyTracking"), "baseNote": settings.get("baseNote")}))
    }

    fn put_back(&self, set: Set) {
        let mut sets = self.sets.borrow_mut();
        if let Some(slot) = sets.iter_mut().find(|held| held.id == set.id) {
            *slot = set;
        }
    }
}

fn pick_set<'a>(sets: &'a [Set], input: &JsonObject) -> Result<&'a Set, String> {
    match input.get("set").and_then(Value::as_u64) {
        Some(id) => sets.iter().find(|set| set.id as u64 == id).ok_or(format!("there is no set {id} (make one first)")),
        None => sets.last().ok_or("there are no takes yet: make some first".into()),
    }
}

fn names_of(input: &JsonObject, default: &[&str]) -> Vec<String> {
    let names: Vec<String> =
        input.get("names").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).map(str::to_owned).collect();
    if names.is_empty() {
        default.iter().map(|name| name.to_string()).collect()
    } else {
        names
    }
}

fn recipes_of(input: &JsonObject, has_source: bool) -> Result<Vec<Recipe>, String> {
    let given: Vec<Recipe> =
        input.get("takes").and_then(Value::as_array).into_iter().flatten().map(Recipe::parse).collect::<Result<_, _>>()?;
    let count = input.get("count").and_then(Value::as_u64).map(|n| n as usize);
    if given.is_empty() && !has_source {
        return Err("new phrases for an instrument need takes with lines you write (later takes can start from: \"V1\")".into());
    }
    let wanted = count.unwrap_or(if given.is_empty() { 4 } else { given.len() });
    if wanted > MOST_TAKES {
        return Err(format!("at most {MOST_TAKES} takes at once"));
    }
    let mut recipes = given;
    let used: Vec<String> = recipes.iter().map(|recipe| recipe.direction.clone()).collect();
    let mut free = DIRECTIONS.iter().filter(|d| !used.iter().any(|u| u == *d));
    while recipes.len() < wanted {
        let direction = free.next().copied().unwrap_or("swapped slices");
        let mut recipe = Recipe::of_direction(direction);
        // (an instrument's new phrases have no source: the takes Kumi picks vary the first one written)
        if !has_source {
            recipe.from = Some(1);
        }
        recipes.push(recipe);
    }
    Ok(recipes)
}

fn bounds_of(input: &JsonObject) -> Bounds {
    match input.get("novelty").and_then(Value::as_str) {
        Some("subtle") => Bounds { min_source: 0.03, max_source: 0.6, min_others: 0.03 },
        Some("bold") => Bounds { min_source: 0.2, max_source: 0.95, min_others: 0.1 },
        _ => Bounds::default(),
    }
}

fn fresh_seed() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|elapsed| elapsed.as_nanos() as u64).unwrap_or(1)
}

/// The anchors given (as Renoise numbers lines) within first..=last, from 0 there.
fn anchors_in(anchors: &[u32], first: u32, last: u32) -> Result<std::collections::BTreeSet<u32>, String> {
    anchors
        .iter()
        .map(|line| {
            if *line < first || *line > last {
                Err(format!("anchor line {line:02} is outside the source's lines ({first:02} to {last:02})"))
            } else {
                Ok(line - first)
            }
        })
        .collect()
}

/// The instrument most of the lines' notes play (from 00), if any.
fn most_played(lines: &Lines) -> Option<u8> {
    let mut counts: BTreeMap<u8, usize> = BTreeMap::new();
    for line in lines.rows.values() {
        for note in &line.notes {
            if let (Some(instrument), true) = (note.instrument, note_value(&note.note).is_some()) {
                *counts.entry(instrument).or_default() += 1;
            }
        }
    }
    counts.into_iter().max_by_key(|(instrument, count)| (*count, std::cmp::Reverse(*instrument))).map(|(instrument, _)| instrument)
}

/// A pattern's lines as a phrase of instrument `index` (from 00) holds them: its notes play the phrase's instrument, so the
/// instrument column goes; a note of another instrument can't be in it.
fn for_phrase(lines: &Lines, index: u8) -> Result<Lines, String> {
    let mut out = lines.clone();
    for (number, line) in out.rows.iter_mut() {
        for note in &mut line.notes {
            match note.instrument {
                Some(instrument) if instrument != index => {
                    return Err(format!("line {number:02} plays instrument {instrument:02X}, not {index:02X}: a phrase plays its own instrument (or name the instrument with phrase.instrumentRef)"))
                }
                _ => note.instrument = None,
            }
        }
    }
    Ok(out)
}

fn first_note(lines: &Lines) -> Option<u8> {
    lines.rows.values().flat_map(|line| line.notes.iter()).find_map(|note| note_value(&note.note))
}

fn phrase_settings(given: &JsonObject, lines: u32, lpb: u32) -> JsonObject {
    let mut settings = JsonObject::new();
    settings.insert("lines".into(), json!(given.get("lines").and_then(Value::as_u64).unwrap_or(lines as u64)));
    settings.insert("lpb".into(), json!(given.get("lpb").and_then(Value::as_u64).unwrap_or(lpb as u64)));
    for key in ["keyTracking", "baseNote"] {
        if let Some(value) = given.get(key) {
            settings.insert(key.into(), value.clone());
        }
    }
    settings
}

/// How long the lines the takes write need their phrases to be: past the last line any take writes, in bars of 16 lines.
fn written_length(input: &JsonObject, least: u32) -> u32 {
    let last = input
        .get("takes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|take| take.get("lines").and_then(Value::as_array))
        .flatten()
        .filter_map(|line| line.get("line").and_then(Value::as_u64))
        .max();
    match last {
        Some(last) => ((last as u32 + 1).div_ceil(16) * 16).clamp(least, 512),
        None => least,
    }
}

fn empty_lines(set: &Set) -> Lines {
    match &set.source.placement {
        Placement::Phrases { settings, .. } => empty_lines_of(
            settings.get("lines").and_then(Value::as_u64).unwrap_or(16) as u32,
            settings.get("lpb").and_then(Value::as_u64).unwrap_or(4) as u32,
        ),
        Placement::Patterns { .. } => empty_lines_of(16, 4),
    }
}

fn empty_lines_of(length: u32, lpb: u32) -> Lines {
    Lines { length: length.max(1), lpb: lpb.max(1), rows: BTreeMap::new(), anchors: Default::default() }
}

/// What finds a take now: its phrase's place, or its pattern's.
fn take_item(info: &SetInfo, place: &Value) -> Value {
    match info.placement {
        Placement::Phrases { .. } => json!({"phrase": place}),
        Placement::Patterns { .. } => json!({"pattern": place}),
    }
}

fn instrument_of(phrase: &str) -> Option<String> {
    let mut parts = phrase.splitn(3, ':');
    let (epoch, kind, numbers) = (parts.next()?, parts.next()?, parts.next()?);
    (kind == "phrase").then(|| format!("{epoch}:instrument:{}", numbers.split('.').next().unwrap_or("1")))
}

fn direction_of(recipe: &Recipe) -> &str {
    if recipe.direction.is_empty() {
        "a variation"
    } else {
        &recipe.direction
    }
}

fn named(row: &Value, otherwise: &str) -> String {
    row.get("name").and_then(Value::as_str).filter(|name| !name.is_empty()).map(str::to_owned).unwrap_or_else(|| otherwise.into())
}

fn clip(text: &str, most: usize) -> String {
    text.chars().take(most).collect()
}

fn take_name(number: usize, source: &str) -> String {
    clip(&format!("V{number} \u{b7} {source}"), NAME_MOST)
}

/// A take's line: "V4 (more reverses): reversed lines 06, 14 · 0.24 from the source (c12)".
fn take_line(drawn: &Drawn, change: &str) -> String {
    let what = if drawn.said.is_empty() { "as drawn".to_string() } else { drawn.said.join("; ") };
    let distance = drawn.distance.map(|d| format!(" \u{b7} {d:.2} from the source")).unwrap_or_default();
    let change = if change.is_empty() { String::new() } else { format!(" ({change})") };
    format!("V{} ({}): {what}{distance}{change}", drawn.number, drawn.direction)
}

struct VariationsTool<S: RefScheme + 'static> {
    owner: Rc<RenoiseVariations<S>>,
}

#[async_trait(?Send)]
impl<S: RefScheme + 'static> KernelTool for VariationsTool<S> {
    fn name(&self) -> &str {
        "variations"
    }

    fn description(&self) -> &str {
        DESCRIPTION
    }

    fn input_schema(&self) -> JsonObject {
        serde_json::from_str(SCHEMA).expect("the variations schema is JSON")
    }

    async fn execute(&self, input: JsonObject, signal: Signal) -> Result<ToolResult, RuntimeError> {
        Ok(match self.owner.run(input, signal).await {
            Ok(answer) => ToolResult::text(stringify(&answer)),
            Err(why) => ToolResult::error(why),
        })
    }

    fn reach(&self) -> Reach {
        Reach::DawWrite
    }
}

#[cfg(test)]
mod tests {
    use super::super::renoise_lines::OPS;
    use super::*;

    #[test]
    fn the_schema_is_json_and_every_operator_it_offers_is_one_kumi_has() {
        let schema: Value = serde_json::from_str(SCHEMA).unwrap();
        let ops = schema.pointer("/properties/takes/items/properties/ops/items/properties/op/enum").unwrap().as_array().unwrap();
        for op in ops {
            let op = op.as_str().unwrap();
            let value = if op == "transpose" { json!({"op": op, "semitones": 2}) } else { json!({"op": op}) };
            assert!(Op::parse(&value).is_ok(), "{op}");
            assert!(OPS.contains(op), "{op} is named in the operators' words");
        }
        for direction in DIRECTIONS {
            assert!(!Recipe::of_direction(direction).ops.is_empty(), "{direction}");
        }
    }

    #[test]
    fn a_patterns_lines_become_a_phrase_of_the_instrument_they_play() {
        let rows = vec![
            json!({"line": 0, "notes": [{"col": 1, "note": "C-4", "instrument": "02"}]}),
            json!({"line": 2, "notes": [{"col": 1, "note": "E-4", "instrument": "02", "volume": "40"}]}),
        ];
        let lines = Lines::from_rows(&rows, 0, 7, 4);
        assert_eq!(most_played(&lines), Some(2));
        let phrase = for_phrase(&lines, 2).unwrap();
        assert!(phrase.rows.values().all(|line| line.notes.iter().all(|note| note.instrument.is_none())));
        assert_eq!(first_note(&phrase), Some(48));
        assert!(for_phrase(&lines, 1).unwrap_err().contains("plays instrument 02"));
    }

    #[test]
    fn takes_are_named_and_described_in_one_line() {
        let drawn = Drawn {
            number: 4,
            material: empty_lines_of(16, 4),
            direction: "more reverses".into(),
            said: vec!["reversed lines 06, 14".into()],
            distance: Some(0.24),
        };
        assert_eq!(take_line(&drawn, "c12"), "V4 (more reverses): reversed lines 06, 14 \u{b7} 0.24 from the source (c12)");
        assert_eq!(take_name(3, "Amen chop"), "V3 \u{b7} Amen chop");
        assert_eq!(instrument_of("5:phrase:2.3").as_deref(), Some("5:instrument:2"));
        assert_eq!(written_length(&json!({"takes": [{"lines": [{"line": 20}]}]}).as_object().unwrap().clone(), 16), 32);
    }

    #[test]
    fn kumi_picks_distinct_directions_for_the_takes_it_is_left() {
        let input = json!({"takes": [{"direction": "sparser", "ops": [{"op": "thin"}]}], "count": 4});
        let recipes = recipes_of(input.as_object().unwrap(), true).unwrap();
        let directions: Vec<&str> = recipes.iter().map(|recipe| recipe.direction.as_str()).collect();
        assert_eq!(directions, ["sparser", "denser", "more reverses", "shifted accents"]);
        assert!(recipes_of(json!({}).as_object().unwrap(), false).unwrap_err().contains("lines you write"));
    }
}
