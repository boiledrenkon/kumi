//! The reads of envelopes M4 adds: a lane's points (`point`: an envelope's own, or an automation item's pool), an
//! envelope's automation items (`autoitem`), and a take's envelopes (an `envelope` read with a take, an item or a take FX
//! as parent). Every envelope row also says its units and how many automation items it has.

use serde_json::{json, Value};

use super::{bad, failed, Pager, Reads};
use crate::changes::{envelope_extras, item_rows, point_source, Ctx};
use crate::queue::Answer;
use crate::reaper::{Envelope, FxOwner, Project, Reaper, Take};
use crate::refs::{self, Ref, TrackId};
use crate::rows;

impl Reads {
    fn ctx<'a, 'r>(&'a self, reaper: &'a Reaper<'r>, proj: Project) -> Ctx<'a, 'r> {
        Ctx { reaper, reads: self, proj }
    }

    /// `read {kind: "point", parent}`: a lane's points in order, in the envelope's units.
    pub(super) fn read_points(
        &self,
        reaper: &Reaper,
        proj: Project,
        mut pager: Pager,
        parent: Option<&str>,
        cursor: Option<&str>,
    ) -> Answer {
        let parent = parent.ok_or_else(|| bad("point: pass the envelope, or an automation item, as parent"))?;
        let ctx = self.ctx(reaper, proj);
        let source = point_source(&ctx, parent)?;
        let mut position = self.position_within(cursor, source.len())?;
        let mut next = None;
        while position < source.len() {
            if !pager.open() || !pager.push(source.row(&ctx, position)) {
                next = Some(position);
                break;
            }
            position += 1;
        }
        let mut answer = self.page(pager, next, Some(source.len()))?;
        answer["units"] = source.units().into();
        Ok(answer)
    }

    /// `read {kind: "autoitem", parent}`: an envelope's automation items, by the refs that name them.
    pub(super) fn read_auto_items(
        &self,
        reaper: &Reaper,
        proj: Project,
        mut pager: Pager,
        parent: Option<&str>,
        cursor: Option<&str>,
    ) -> Answer {
        let parent = parent.ok_or_else(|| bad("autoitem: pass the track envelope as parent"))?;
        let rows = item_rows(&self.ctx(reaper, proj), parent)?;
        let mut position = self.position_within(cursor, rows.len())?;
        let mut next = None;
        while position < rows.len() {
            if !pager.open() || !pager.push(rows[position].clone()) {
                next = Some(position);
                break;
            }
            position += 1;
        }
        self.page(pager, next, Some(rows.len()))
    }

    /// A track envelope's row, with its units and automation items.
    pub(super) fn track_envelope_row(&self, reaper: &Reaper, proj: Project, track: &TrackId, envelope: Envelope) -> Value {
        let mut row = rows::envelope_row(reaper, track, envelope);
        merge(&mut row, envelope_extras(&self.ctx(reaper, proj), envelope));
        row
    }

    /// A take envelope's row (a take's volume, pan, mute, pitch, or one of its FX parameters).
    pub(super) fn take_envelope_row(&self, reaper: &Reaper, proj: Project, take: Take, envelope: Envelope) -> Value {
        let guid = reaper.envelope_guid(envelope).unwrap_or_default();
        let mut row = json!({
            "ref": (!guid.is_empty()).then(|| refs::take_envelope_ref(&reaper.take_guid(take), &guid)),
            "name": rows::clip(&reaper.envelope_name(envelope)),
            "pointCount": reaper.envelope_point_count(envelope),
            "armed": reaper.envelope_flag(envelope, "ARM"),
            "visible": reaper.envelope_flag(envelope, "VISIBLE"),
            "active": reaper.envelope_flag(envelope, "ACTIVE"),
            "takeRef": refs::take_ref(&reaper.take_guid(take)),
        });
        merge(&mut row, envelope_extras(&self.ctx(reaper, proj), envelope));
        row
    }

    /// `read {kind: "envelope", parent: take | item | take FX}`: a take's envelopes (a take FX's only its own).
    pub(super) fn read_take_envelopes(
        &self,
        reaper: &Reaper,
        proj: Project,
        mut pager: Pager,
        parent: &str,
        cursor: Option<&str>,
    ) -> Answer {
        let (take, only): (Take, Option<Vec<Envelope>>) = match Ref::parse(parent).map_err(bad)? {
            Ref::Take(guid) => (self.take_by_ref(reaper, proj, &guid)?, None),
            Ref::Item(guid) => {
                let item = self.item_by_ref(reaper, proj, &guid)?;
                (reaper.active_take(item).ok_or_else(|| failed("that item has no take"))?, None)
            }
            Ref::Fx(guid) => {
                let fx = self.fx_by_guid(reaper, proj, &guid)?;
                let FxOwner::Take(take) = fx.owner else { return Err(bad("that FX is a track's")) };
                let own = (0..reaper.fx_param_count(fx)).filter_map(|p| reaper.fx_envelope(fx, p, false)).collect();
                (take, Some(own))
            }
            other => return Err(bad(format!("parent: a take, item or FX ref, not r:{}", other.kind()))),
        };
        let found: Vec<Envelope> = (0..reaper.take_envelope_count(take))
            .filter_map(|i| reaper.take_envelope(take, i))
            .filter(|e| only.as_ref().is_none_or(|own| own.contains(e)))
            .collect();
        let key = |position: usize| found.get(position).and_then(|envelope| reaper.envelope_guid(*envelope));
        let mut position = self.resume_number(cursor, found.len(), key)?;
        let mut next = None;
        while position < found.len() {
            if !pager.open() || !pager.push(self.take_envelope_row(reaper, proj, take, found[position])) {
                next = Some(position);
                break;
            }
            position += 1;
        }
        self.page_keyed(pager, next, Some(found.len()), key)
    }

    /// `read {ref}` of a take envelope or an automation item.
    pub(super) fn read_one_m4(&self, reaper: &Reaper, proj: Project, reference: &Ref) -> Result<Value, (String, String)> {
        let ctx = self.ctx(reaper, proj);
        match reference {
            Ref::TakeEnvelope { take, guid } => {
                let take = self.take_by_ref(reaper, proj, take)?;
                let envelope = (0..reaper.take_envelope_count(take))
                    .filter_map(|i| reaper.take_envelope(take, i))
                    .find(|e| reaper.envelope_guid(*e).is_some_and(|g| g.eq_ignore_ascii_case(guid)))
                    .ok_or_else(|| failed("that take has no such envelope (deleted, or another take)"))?;
                Ok(self.take_envelope_row(reaper, proj, take, envelope))
            }
            Ref::AutoItem { track, envelope, .. } => {
                let parent = refs::envelope_ref(track, &refs::EnvelopeId::Guid(envelope.clone()));
                let text = reference.text();
                item_rows(&ctx, &parent)?.into_iter().find(|row| row["ref"].as_str() == Some(text.as_str())).ok_or_else(|| {
                    failed("that automation item isn't there (moved, or deleted): read the envelope's automation items again")
                })
            }
            _ => Err(bad("not an envelope or automation item ref")),
        }
    }
}

fn merge(row: &mut Value, extra: Value) {
    if let (Some(row), Value::Object(extra)) = (row.as_object_mut(), extra) {
        row.extend(extra);
    }
}
