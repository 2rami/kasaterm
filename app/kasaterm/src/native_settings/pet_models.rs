use super::*;

pub(super) fn paint(g: &mut gpu::GpuRenderer, s: &Snapshot, hits: &mut Vec<Hit>, x: f32, y: &mut f32, w: f32) {
    use crate::chrome::PetModelState as State;
    let models = crate::settings::pet_models::models();
    for model in &models {
        if !matches!(model.state, State::Installed | State::Downloading {..}) {
            if let Some(hint) = crate::chrome::pet_model_source_hint(&model.name) {
                plain_hint(g, x, y, w, hint);
                *y += button_row(g, s, hits, (x, *y, w, 0.0), &[
                    ("받은 ZIP 추가", Target::Setting(SettingsAction::PetImport(model.name.clone(), false)), false),
                    ("모델 폴더 추가", Target::Setting(SettingsAction::PetImport(model.name.clone(), true)), false),
                ], crate::native_layout::Align::Start) + 12.0;
            }
        }
    }
    let gap = 8.0;
    let cols = (((w + gap) / 148.0).floor() as usize).max(1);
    let width = ((w - (cols - 1) as f32 * gap) / cols as f32).max(1.0);
    let heights: Vec<f32> = models.chunks(cols).map(|row| if row.iter().any(|model| matches!(model.state, State::Installed)) { 196.0 } else { 104.0 }).collect();
    for (index, model) in models.iter().enumerate() {
        let row = index / cols;
        let height = heights[row];
        let top = *y + heights[..row].iter().map(|height| height + gap).sum::<f32>();
        let rect = (x + (index % cols) as f32 * (width + gap), top, width, height);
        let installed = matches!(model.state, State::Installed);
        if installed {
            choice_card(g, s, hits, rect, model.selected, Target::Setting(SettingsAction::PetCharacter(model.name.clone())));
        } else {
            stroke_round(g, rect, theme::radius_md(), if model.selected { theme::accent() } else { theme::border() });
        }
        let image_w = (width - 24.0).min(80.0).max(1.0);
        let image = (rect.0 + (width - image_w) / 2.0, rect.1 + 8.0, image_w, 110.0);
        let mut drawn = false;
        if let Some(path) = crate::chrome::pet_preview_path(&model.name) {
            let key = format!("pet:{}", model.name);
            if !g.has_image(&key) {
                if let Some((rgba, iw, ih)) = crate::sprites::user_asset_rgba_in(path.parent().unwrap_or(&path), "preview.png") {
                    g.upload_image(&key, &rgba, iw, ih);
                }
            }
            if g.has_image(&key) { g.queue_image_above(&key, image.0, image.1, image.2, image.3); drawn = true; }
        }
        if !drawn { g.queue_icon(if installed { "sparkles" } else { "arrow-down" }, rect.0 + (width - 20.0) / 2.0, rect.1 + if height > 104.0 { 48.0 } else { 10.0 }, 20.0, theme::text_mute()); }
        let label = if model.default { format!("{} · 기본", model.label) } else { model.label.clone() };
        let label = fit(g, &label, width - 16.0, 12.0, model.selected);
        let tw = g.measure_chrome_text(&label, 12.0, model.selected);
        draw_text(g, rect.0 + (width - tw) / 2.0, rect.1 + height - 71.0, &label, 12.0, theme::text(), model.selected);
        let (status, action) = match &model.state {
            State::Installed => (if model.selected { "선택됨".into() } else { "사용하기".into() }, None),
            State::Downloading { done, total } => (if *total > 0 { format!("받는 중 · {}%", done.saturating_mul(100) / total) } else { "받는 중…".into() }, None),
            State::Failed(_) | State::Missing if model.can_download => (String::new(), Some((if matches!(model.state, State::Failed(_)) { "다시 받기" } else { "다운로드" }, SettingsAction::PetDownload(model.name.clone())))),
            _ => (String::new(), Some(("받는 곳", SettingsAction::PetSource(model.name.clone())))),
        };
        if let Some((label, action)) = action {
            let button_w = crate::native_controls::button_width(g, label, 32.0, false).min(width - 16.0);
            button(g, s, hits, (rect.0 + (width - button_w) / 2.0, rect.1 + height - 43.0, button_w, 32.0), label, Target::Setting(action), false);
        } else {
            let shown = fit(g, &status, width - 16.0, 11.0, false);
            let tw = g.measure_chrome_text(&shown, 11.0, false);
            draw_text(g, rect.0 + (width - tw) / 2.0, rect.1 + height - 31.0, &shown, 11.0, theme::text_dim(), false);
        }
    }
    *y += heights.iter().map(|height| height + gap).sum::<f32>() + 8.0;
    for model in &models {
        if let State::Failed(error) = &model.state { plain_hint(g, x, y, w, &format!("{}: {error}", model.label)); }
    }
}
