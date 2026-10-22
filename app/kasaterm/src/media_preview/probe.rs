use super::*;
impl State {
    pub(super) fn observe(&mut self, owner: WindowId, g: &gpu::GpuRenderer) {
        if !crate::verification_run() {
            return;
        }
        let Some(path) = std::env::var_os("KASATERM_TEST_MEDIA_REPORT") else {
            return;
        };
        if self
            .probe_at
            .is_some_and(|at| at.elapsed() < std::time::Duration::from_millis(100))
        {
            return;
        }
        self.probe_at = Some(Instant::now());
        let hits=self.hits.get(&owner).map(|(_,hits)|hits.iter().map(|h|{
            let asset=cache::get(&h.reference);
            serde_json::json!({"id":h.reference.id,"rect":h.rect,"ready":asset.as_ref().is_ok_and(|v|v.is_some()),"error":asset.err(),
                "controls":h.controls.iter().map(|(c,r)|serde_json::json!({"control":format!("{c:?}"),"rect":r})).collect::<Vec<_>>()})
        }).collect::<Vec<_>>()).unwrap_or_default();
        let session=|s:&super::Session|{
            let state=s.state().map(|s|serde_json::json!({"position":s.position,"duration":s.duration,"playing":s.playing,"buffering":s.buffering,"seeking":s.seeking,"ended":s.ended,"error":s.error}));
            serde_json::json!({"id":s.reference.id,"state":state,"error":s.error,"received":s.stream.as_ref().map(Stream::received_bytes),"stream_error":s.stream.as_ref().and_then(Stream::error),
                "size":s.asset.as_ref().map(|a|a.media.size),"muted":s.muted,"frames":s.frame_serial,"drawn":g.drawn_image_clipped_rects(&s.key())})
        };
        let preview=self.preview.as_ref().filter(|p|p.owner==owner).map(|p|{
            let mut v=session(&p.session);
            v["seek"]=serde_json::json!(p.seek);
            v["from_card"]=serde_json::json!(p.from_card);
            v["actions"]=serde_json::json!(p.actions.iter().map(|(a,r)|serde_json::json!({"action":format!("{a:?}"),"rect":r})).collect::<Vec<_>>());
            v
        });
        let inline=self.inline.as_ref().filter(|i|i.owner==owner).map(|i|session(&i.session));
        let saving=self.saving.lock().map(|s|s.len()).unwrap_or(0);
        let _ = std::fs::write(
            path,
            serde_json::to_vec(&serde_json::json!({"hits":hits,"preview":preview,"inline":inline,"saving":saving})).unwrap(),
        );
    }
}
