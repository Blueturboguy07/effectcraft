//! Headless look at Layer ▸ AI's dialogs and Settings ▸ AI (wgpu offscreen). Ignored by default
//! (needs a GPU adapter); run with
//! `AI_SNAPSHOT_DIR=/abs/dir cargo test -p effectcraft-ui-egui --test ai_snapshot -- --ignored`.
//! Nothing is sent: the dialogs only open, and the key is a placeholder.

use effectcraft_ui_egui::ai::{Ai, AppInfo};
use effectcraft_ui_egui::{Dialog, EffectcraftApp};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use serde_json::json;

#[test]
#[ignore]
fn ai_dialogs_snapshot() {
    let out = std::env::var("AI_SNAPSHOT_DIR").unwrap_or_else(|_| concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/test-out").into());
    let out = std::path::PathBuf::from(out);
    std::fs::create_dir_all(&out).unwrap();
    let config = out.join("ai-snapshot-config");
    let _ = std::fs::remove_dir_all(&config);
    let mut s = effectcraft_host::session();
    s.execute("comp.new", json!({"name": "Main", "width": 1920, "height": 1080, "duration": 10})).unwrap();
    s.execute("time.set", json!({"time": 2.5})).unwrap();
    let mut app = EffectcraftApp::new(s);
    let info = AppInfo {
        slug: "effectcraft-snapshot",
        name: "EffectCraft",
        version: "0.0.0",
        app_token: "",
        dialects: &["images", "audio_speech"],
        config_dir: Some(config.clone()),
    };
    let mut ai = Ai::new(info);
    ai.skip_first_run();
    ai.state.settings.provider = "own".into();
    ai.state.settings.own_key = "sk-placeholder".into();
    app.ai = Some(ai);
    let mut app = Some(app);
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).build_eframe(|_| app.take().expect("app"));
    for (name, open) in [("ai_image.png", "ai.generateImageLayer"), ("ai_voice.png", "ai.voiceOver"), ("ai_settings.png", "ai.settings")] {
        h.state_mut().dialog = None;
        h.run_steps(2);
        let ctx = h.ctx.clone();
        effectcraft_ui_egui::menus::invoke(h.state_mut(), &ctx, open, json!({})).unwrap();
        h.run_steps(2);
        // Text typed: Generate can be clicked (it is not).
        if open != "ai.settings" {
            h.get_by_role(egui::accesskit::Role::MultilineTextInput).type_text("A lighthouse on a cliff at dawn");
        }
        h.run_steps(4);
        assert!(h.state().dialog.is_some_and(|d| matches!(d, Dialog::AiImage | Dialog::AiVoice | Dialog::Settings)), "{open}");
        h.render().expect("render").save(out.join(name)).unwrap();
    }
    assert!(!h.state().ai.as_ref().unwrap().generating(), "opening the dialogs sent nothing");
    let _ = std::fs::remove_dir_all(&config);
}
