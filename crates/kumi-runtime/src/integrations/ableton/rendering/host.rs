//! Live as the listening tools meet it: what `Renderer` asks, answered with Live's bridge, its render rigs and Kumi
//! Ears. The renderer is the `Rc` itself, since the rigs are made and closed through it.
//!
//! Inside this impl the inherent methods are called by their path (`LiveRenderer::tell(self, …)`): a method call on the
//! `Rc` finds `Renderer`'s first.
use super::super::connection::NO_CURRENT_LIVE;
use super::{rig::BEFORE_MAIN, *};
use crate::{
    core::daw::DawProfile,
    listening::{
        checklist::MainState,
        judging::Placed,
        renderer::{Captured, ListenFlag, Pass, Renderer, Window},
    },
};
use async_trait::async_trait;
use kumi_common::js::string::head;

#[async_trait(?Send)]
impl Renderer for Rc<LiveRenderer> {
    fn profile(&self) -> &DawProfile {
        &DawProfile::ABLETON
    }
    fn refusal(&self) -> Option<String> {
        (!LiveRenderer::available(self)).then(|| NO_CURRENT_LIVE.to_string())
    }
    fn tempo(&self) -> Option<f64> {
        self.observer.tempo.get()
    }
    fn beats_per_bar(&self) -> f64 {
        self.observer.beats_per_bar.get()
    }
    fn lifetime(&self) -> Signal {
        LiveRenderer::connection(self).lifetime.clone()
    }
    fn cleanup(&self) -> Signal {
        LiveRenderer::cleanup(self)
    }
    fn tell(&self, title: &str, playing: Option<bool>) {
        LiveRenderer::tell(self, title, playing)
    }
    fn flag(&self) -> &ListenFlag {
        &self.rendering
    }
    fn kept_folder(&self) -> PathBuf {
        self.ears_folder.join("judge")
    }

    async fn song_end(&self, signal: Signal) -> Option<f64> {
        LiveRenderer::song_end(self, signal).await
    }
    async fn track_name(&self, named: &str, signal: Signal) -> Option<String> {
        LiveRenderer::track_name(self, named, signal).await
    }
    async fn track_ref(&self, named: &str, signal: Signal) -> Option<String> {
        self.track_ref_long(named, signal).await
    }
    async fn clip_file(&self, named: &str, signal: Signal) -> Result<Option<String>, RuntimeError> {
        (self.clip_file)(named.into(), signal).await
    }

    async fn hear_pass(&self, candidates: &[AuditionCandidate], window: Window, before_main: bool, signal: Signal) -> Pass {
        let mut rig = None;
        let rendered: Result<(IndexMap<String, Render>, Vec<(String, bool)>), RuntimeError> = async {
            rig = Some(self.open_rig_with(candidates, Some(window.from), Some(window.beats), before_main, signal.clone()).await?);
            let rig = rig.as_mut().unwrap();
            let files = self.render_pass(rig, signal.clone()).await?;
            Ok((files, rig.sources.iter().map(|source| (source.name.clone(), source.mix)).collect()))
        }
        .await;
        let mut notes = vec![];
        // Why the mix wasn't heard as it comes into Main's chain, when Kumi Ears tried to: its device wasn't placed, or
        // nothing came through from it.
        let mut tried = None;
        if let Some(rig) = rig.as_mut() {
            let placed = rig.ears.as_ref().is_some_and(|ears| ears.before_main.is_some());
            self.close_rig(rig).await;
            notes.extend(rig.notes.clone());
            tried = rig.before_main_missed.clone().or_else(|| placed.then(|| "nothing came through from it".to_string()));
        }
        let rendered = rendered.map(|(mut files, sources)| {
            let before_master = files.shift_remove(BEFORE_MAIN);
            Captured { files, sources, before_master }
        });
        Pass { rendered, notes, tried }
    }
    async fn master_chain(&self, signal: Signal) -> MainState {
        self.main_state(signal).await
    }
    async fn fader_of(&self, track: &str, signal: Signal) -> Option<f64> {
        LiveRenderer::fader_of(self, track, signal).await
    }

    fn applied(&self) -> Vec<String> {
        LiveRenderer::applied_ids(self)
    }
    fn applied_since(&self, mark: &[String]) -> Vec<(String, String)> {
        LiveRenderer::applied_since(self, mark)
    }
    fn audible(&self, id: &str) -> Option<bool> {
        self.audible_change(id)
    }
    fn outside(&self) -> usize {
        self.history.outside.borrow().len()
    }
    fn outside_since(&self, mark: usize) -> Vec<&'static str> {
        self.history.outside_since(mark)
    }
    async fn undo(&self, id: &str) -> Result<(), String> {
        self.undo_change(id).await
    }
    fn made(&self, changes: &[(String, String)]) -> Vec<String> {
        self.devices_made_by(changes)
    }
    fn devices_made(&self, changes: &[(String, String)]) -> usize {
        self.created_by(changes)
    }
    fn renames(&self, before: &[String]) -> Vec<(String, String, String)> {
        self.renames_since(before)
    }

    async fn turn_gain(&self, gain: f64, track: Option<&str>, signal: Signal) -> Result<(String, bool), String> {
        self.rebalance(gain, track, signal).await
    }
    async fn limited(&self, track: Option<&str>, signal: Signal) -> bool {
        matches!(self.find_gain_stage(track, signal).await, Ok(Some(stage)) if stage.limited)
    }
    async fn chain(&self, track: Option<&str>) -> Result<Vec<Placed>, String> {
        self.chain_now(track, LiveRenderer::cleanup(self)).await.map_err(|error| head(&error.to_string(), 120))
    }
    async fn delete_device(&self, reference: &str) -> bool {
        self.step("delete_device", json!({"ref":reference}), LiveRenderer::cleanup(self)).await.is_ok()
    }
}
