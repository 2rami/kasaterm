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
            serde_json::json!({"id":h.reference.id,"rect":h.rect,"ready":asset.as_ref().is_ok_and(|v|v.is_some()),"error":asset.err()})
        }).collect::<Vec<_>>()).unwrap_or_default();
        let preview=self.preview.as_ref().filter(|p|p.owner==owner).map(|p|{
            let state=p.player.as_ref().map(|player|{let s=player.state();serde_json::json!({"position":s.position,"duration":s.duration,"playing":s.playing,"buffering":s.buffering,"seeking":s.seeking,"ended":s.ended,"error":s.error})});
            serde_json::json!({"state":state,"error":p.error,"received":p.stream.as_ref().map(Stream::received_bytes),"stream_error":p.stream.as_ref().and_then(Stream::error),"size":p.asset.as_ref().map(|a|a.media.size),"muted":p.muted,"frames":p.frame_serial,
                "seek":p.seek,"drawn":g.drawn_image_clipped_rects(&format!("media-preview:{}",p.generation)),
                "actions":p.actions.iter().map(|(a,r)|serde_json::json!({"action":format!("{a:?}"),"rect":r})).collect::<Vec<_>>()})
        });
        let _ = std::fs::write(
            path,
            serde_json::to_vec(&serde_json::json!({"hits":hits,"preview":preview})).unwrap(),
        );
    }
}
