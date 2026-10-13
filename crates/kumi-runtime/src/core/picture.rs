//! What a change looks like, before and after, for the producer to check by eye: the values that moved, an
//! equaliser's curve, a compressor's curve, a clip's notes, an automation lane or a channel's levels. Plain,
//! DAW-neutral data: an integration fills it from its own records, and the terminal draws it knowing nothing of
//! the DAW. Kept small (at most [`MAX_BYTES`] as JSON), since saved conversations keep thousands of changes.

use serde::{Deserialize, Serialize};

use super::contracts::ClipNote;

/// The most one change's picture takes as JSON: past it, lists are thinned until it fits.
pub const MAX_BYTES: usize = 4096;
const MAX_VALUES: usize = 16;
const MAX_BANDS: usize = 16;
const MAX_NOTES: usize = 256;
const MAX_POINTS: usize = 128;
const MAX_SENDS: usize = 12;
const MAX_NAME: usize = 64;
const MAX_TEXT: usize = 32;

/// A change's picture, by kind.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ChangePicture {
    /// Each value that moved: a device's parameters, the tempo.
    Values { values: Vec<PictureValue> },
    /// An equaliser's bands, before and after, with the values that moved.
    Eq {
        before: Vec<EqBand>,
        after: Vec<EqBand>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        values: Vec<PictureValue>,
    },
    /// A compressor's curve (level in against level out), before and after, with the values that moved.
    Dynamics {
        before: Dynamics,
        after: Dynamics,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        values: Vec<PictureValue>,
    },
    /// A clip's notes before and after, in beats from its start; `length` is the clip's.
    Notes { length: f64, before: Vec<ClipNote>, after: Vec<ClipNote> },
    /// An automation lane's points ([beat, value]) before and after, in the parameter's range.
    Automation {
        #[serde(default, skip_serializing_if = "String::is_empty")]
        parameter: String,
        min: f64,
        max: f64,
        before: Vec<[f64; 2]>,
        after: Vec<[f64; 2]>,
    },
    /// A channel's fader, pan and sends, before and after.
    Mix { before: MixLevels, after: MixLevels },
}

/// One value that moved, as numbers in its range and as the DAW shows it, and what was asked when the DAW set
/// something else.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PictureValue {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    /// "-3.0 dB" → "-1.5 dB".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_text: Option<String>,
    /// What was asked, when the DAW set something else: "2 dB" where it set "1.9 dB".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asked: Option<String>,
}

/// One equaliser band.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EqBand {
    pub shape: EqShape,
    pub freq: f64,
    #[serde(default)]
    pub gain_db: f64,
    #[serde(default = "default_q")]
    pub q: f64,
    #[serde(default = "on")]
    pub on: bool,
    /// A cut's steepness in dB per octave (12, 24, 48); none for the others.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slope: Option<f64>,
}

fn default_q() -> f64 {
    0.71
}

fn on() -> bool {
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EqShape {
    Bell,
    LowShelf,
    HighShelf,
    LowCut,
    HighCut,
    Notch,
}

/// A compressor's static curve.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Dynamics {
    pub threshold_db: f64,
    /// 4 for 4:1; a limiter's is large.
    pub ratio: f64,
    #[serde(default)]
    pub knee_db: f64,
    #[serde(default)]
    pub makeup_db: f64,
}

/// A channel's levels: the fader (0–1 along its travel) and pan (-1 left to 1 right), each with how the DAW shows
/// it, and its sends.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MixLevels {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pan: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pan_text: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sends: Vec<MixSend>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mute: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub solo: Option<bool>,
}

/// One send: its name (the return's) and its level, 0–1.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MixSend {
    pub name: String,
    pub level: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

impl ChangePicture {
    /// The picture within its bounds: lists capped, names and texts cut, numbers finite, and thinned until its JSON
    /// is at most [`MAX_BYTES`]. None when nothing drawable is left.
    pub fn bounded(self) -> Option<ChangePicture> {
        let mut picture = self.capped()?;
        for _ in 0..8 {
            if serde_json::to_vec(&picture).map_or(usize::MAX, |json| json.len()) <= MAX_BYTES {
                return Some(picture);
            }
            picture = picture.halved()?;
        }
        None
    }

    fn capped(self) -> Option<ChangePicture> {
        let values = |values: Vec<PictureValue>| values.into_iter().take(MAX_VALUES).filter_map(PictureValue::bounded).collect::<Vec<_>>();
        let bands = |bands: Vec<EqBand>| bands.into_iter().filter(EqBand::finite).take(MAX_BANDS).collect::<Vec<_>>();
        let picture = match self {
            ChangePicture::Values { values: list } => ChangePicture::Values { values: values(list) },
            ChangePicture::Eq { before, after, values: list } => {
                ChangePicture::Eq { before: bands(before), after: bands(after), values: values(list) }
            }
            ChangePicture::Dynamics { before, after, values: list } => {
                if !before.finite() || !after.finite() {
                    return None;
                }
                ChangePicture::Dynamics { before, after, values: values(list) }
            }
            ChangePicture::Notes { length, before, after } => {
                let notes = |notes: Vec<ClipNote>| {
                    let mut notes: Vec<ClipNote> = notes.into_iter().filter(finite_note).collect();
                    notes.sort_by(|a, b| a.start.total_cmp(&b.start).then(a.pitch.total_cmp(&b.pitch)));
                    thin(notes, MAX_NOTES)
                };
                ChangePicture::Notes {
                    length: if length.is_finite() && length > 0. { length } else { 0. },
                    before: notes(before),
                    after: notes(after),
                }
            }
            ChangePicture::Automation { parameter, min, max, before, after } => {
                let points = |points: Vec<[f64; 2]>| {
                    let mut points: Vec<[f64; 2]> = points.into_iter().filter(|[at, value]| at.is_finite() && value.is_finite()).collect();
                    points.sort_by(|a, b| a[0].total_cmp(&b[0]));
                    thin(points, MAX_POINTS)
                };
                if !min.is_finite() || !max.is_finite() || max <= min {
                    return None;
                }
                ChangePicture::Automation { parameter: cut(&parameter, MAX_NAME), min, max, before: points(before), after: points(after) }
            }
            ChangePicture::Mix { before, after } => ChangePicture::Mix { before: before.bounded(), after: after.bounded() },
        };
        (!picture.is_empty()).then_some(picture)
    }

    /// Half of each list (the endpoints kept), for a picture still too large.
    fn halved(self) -> Option<ChangePicture> {
        let half = |n: usize| (n / 2).max(1);
        let picture = match self {
            ChangePicture::Values { values } => ChangePicture::Values { values: values.iter().take(half(values.len())).cloned().collect() },
            ChangePicture::Eq { before, after, values } => {
                ChangePicture::Eq { before, after, values: values.iter().take(values.len() / 2).cloned().collect() }
            }
            ChangePicture::Dynamics { before, after, values } => {
                ChangePicture::Dynamics { before, after, values: values.iter().take(values.len() / 2).cloned().collect() }
            }
            ChangePicture::Notes { length, before, after } => ChangePicture::Notes {
                length,
                before: thin(before.clone(), half(before.len())),
                after: thin(after.clone(), half(after.len())),
            },
            ChangePicture::Automation { parameter, min, max, before, after } => ChangePicture::Automation {
                parameter,
                min,
                max,
                before: thin(before.clone(), half(before.len())),
                after: thin(after.clone(), half(after.len())),
            },
            ChangePicture::Mix { .. } => return None,
        };
        (!picture.is_empty()).then_some(picture)
    }

    fn is_empty(&self) -> bool {
        match self {
            ChangePicture::Values { values } => values.is_empty(),
            ChangePicture::Eq { before, after, .. } => before.is_empty() && after.is_empty(),
            ChangePicture::Dynamics { .. } => false,
            ChangePicture::Notes { before, after, .. } => before.is_empty() && after.is_empty(),
            ChangePicture::Automation { before, after, .. } => before.is_empty() && after.is_empty(),
            ChangePicture::Mix { before, after } => before == &MixLevels::default() && after == &MixLevels::default(),
        }
    }
}

impl PictureValue {
    fn bounded(self) -> Option<PictureValue> {
        let finite = |n: Option<f64>| n.filter(|n| n.is_finite());
        let text = |t: Option<String>| t.map(|t| cut(&t, MAX_TEXT)).filter(|t| !t.is_empty());
        let value = PictureValue {
            name: cut(&self.name, MAX_NAME),
            from: finite(self.from),
            to: finite(self.to),
            min: finite(self.min),
            max: finite(self.max),
            from_text: text(self.from_text),
            to_text: text(self.to_text),
            asked: text(self.asked),
        };
        (!value.name.is_empty() && (value.to.is_some() || value.to_text.is_some())).then_some(value)
    }
}

impl EqBand {
    fn finite(&self) -> bool {
        self.freq.is_finite() && self.freq > 0. && self.gain_db.is_finite() && self.q.is_finite() && self.q > 0.
    }
}

impl Dynamics {
    fn finite(&self) -> bool {
        self.threshold_db.is_finite()
            && self.ratio.is_finite()
            && self.ratio >= 1.
            && self.knee_db.is_finite()
            && self.makeup_db.is_finite()
    }
}

impl MixLevels {
    fn bounded(self) -> MixLevels {
        let finite = |n: Option<f64>| n.filter(|n| n.is_finite());
        let text = |t: Option<String>| t.map(|t| cut(&t, MAX_TEXT)).filter(|t| !t.is_empty());
        MixLevels {
            volume: finite(self.volume),
            volume_text: text(self.volume_text),
            pan: finite(self.pan),
            pan_text: text(self.pan_text),
            sends: self
                .sends
                .into_iter()
                .filter(|send| send.level.is_finite())
                .take(MAX_SENDS)
                .map(|send| MixSend { name: cut(&send.name, MAX_NAME), level: send.level, text: text(send.text) })
                .collect(),
            mute: self.mute,
            solo: self.solo,
        }
    }
}

fn finite_note(note: &ClipNote) -> bool {
    note.pitch.is_finite() && note.start.is_finite() && note.duration.is_finite() && note.duration > 0. && note.velocity.is_finite()
}

/// At most `most` of the items, spread evenly over them, the first and last kept.
fn thin<T: Clone>(items: Vec<T>, most: usize) -> Vec<T> {
    if items.len() <= most || most == 0 {
        return if most == 0 { Vec::new() } else { items };
    }
    if most == 1 {
        return vec![items[0].clone()];
    }
    let last = items.len() - 1;
    (0..most).map(|i| items[i * last / (most - 1)].clone()).collect()
}

/// At most `max` characters.
fn cut(text: &str, max: usize) -> String {
    text.trim().chars().take(max).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(start: f64) -> ClipNote {
        ClipNote { pitch: 60. + (start as i64 % 12) as f64, start, duration: 0.25, velocity: 100. }
    }

    #[test]
    fn a_picture_stays_within_its_bounds() {
        let many = ChangePicture::Notes {
            length: 512.,
            before: (0..2000).map(|i| note(i as f64 / 4.)).collect(),
            after: (0..2000).map(|i| note(i as f64 / 4.)).collect(),
        };
        let bounded = many.bounded().unwrap();
        assert!(serde_json::to_vec(&bounded).unwrap().len() <= MAX_BYTES);
        let ChangePicture::Notes { before, after, .. } = &bounded else { panic!() };
        assert!(!before.is_empty() && !after.is_empty());
        assert_eq!(after.first().unwrap().start, 0., "the first note stays");
        assert_eq!(after.last().unwrap().start, 1999. / 4., "and the last");
        let points = ChangePicture::Automation {
            parameter: "Cutoff".into(),
            min: 0.,
            max: 1.,
            before: vec![],
            after: (0..1000).map(|i| [i as f64, 0.5]).collect(),
        };
        let ChangePicture::Automation { after, .. } = points.bounded().unwrap() else { panic!() };
        assert!(after.len() <= MAX_POINTS);
    }

    #[test]
    fn what_cant_be_drawn_is_dropped() {
        let value = |name: &str, to: Option<f64>| PictureValue { name: name.into(), to, ..Default::default() };
        let picture = ChangePicture::Values { values: vec![value("Gain", Some(f64::NAN)), value("", Some(1.)), value("Drive", Some(0.5))] };
        assert_eq!(picture.bounded(), Some(ChangePicture::Values { values: vec![value("Drive", Some(0.5))] }));
        assert_eq!(ChangePicture::Values { values: vec![value("Gain", None)] }.bounded(), None);
        let automation = ChangePicture::Automation { parameter: String::new(), min: 1., max: 1., before: vec![[0., 1.]], after: vec![] };
        assert_eq!(automation.bounded(), None, "an empty range draws nothing");
        let silly = Dynamics { threshold_db: -20., ratio: 0.5, knee_db: 0., makeup_db: 0. };
        assert_eq!(ChangePicture::Dynamics { before: silly.clone(), after: silly, values: vec![] }.bounded(), None);
    }

    #[test]
    fn records_without_a_picture_still_read() {
        let old: super::super::contracts::ChangeRecord = serde_json::from_value(serde_json::json!({
            "id":"c1","family":"tempo","title":"Tempo 120 → 124 BPM","from":120.0,"to":124.0,"state":"applied","at":1
        }))
        .unwrap();
        assert_eq!(old.picture, None);
        let eq = ChangePicture::Eq {
            before: vec![EqBand { shape: EqShape::Bell, freq: 1000., gain_db: 0., q: 1., on: true, slope: None }],
            after: vec![EqBand { shape: EqShape::Bell, freq: 1000., gain_db: -3., q: 1., on: true, slope: None }],
            values: vec![],
        };
        let json = serde_json::to_value(&eq).unwrap();
        assert_eq!(json["kind"], "eq");
        assert_eq!(json["after"][0]["gainDb"], -3.0);
        assert_eq!(serde_json::from_value::<ChangePicture>(json).unwrap(), eq);
    }
}
