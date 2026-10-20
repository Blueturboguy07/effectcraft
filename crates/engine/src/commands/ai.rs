//! Layer ▸ AI (desktop only): AI through publik API or the person's own key. The commands open
//! the desktop app's dialogs (`effectcraft_ui_egui::ai`); they send nothing. A request goes out,
//! and is charged, only when the person clicks Generate in a dialog. Headless sessions accept and
//! ignore them, like the other frontend commands.

use super::{CommandSpec, always, frontend, has_comp};
use crate::cmd;

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            "ai.generateImageLayer",
            "Generate Image Layer...",
            ["Layer", "AI"],
            None,
            "{} — opens the dialog; the image becomes a new layer of the active composition at the current time, centred. Nothing is sent until Generate is clicked",
            has_comp,
            |s, p| frontend(s, "ai.generateImageLayer", p)
        ),
        cmd!(
            "ai.voiceOver",
            "Voice-over from Text...",
            ["Layer", "AI"],
            None,
            "{} — opens the dialog; the speech becomes a new audio layer of the active composition at the current time. Nothing is sent until Generate is clicked",
            has_comp,
            |s, p| frontend(s, "ai.voiceOver", p)
        ),
        cmd!("ai.settings", "AI Settings...", ["Layer", "AI"], None, "{} — Settings on its AI page", always, |s, p| frontend(s, "ai.settings", p)),
    ]
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    /// The commands only ask the frontend for a dialog: nothing in the project changes.
    #[test]
    fn ai_commands_open_dialogs_and_change_nothing() {
        let mut s = crate::Session::default();
        assert!(s.execute("ai.generateImageLayer", json!({})).is_err(), "no composition is open");
        assert!(s.execute("ai.settings", json!({})).is_ok());
        s.execute("comp.new", json!({"name": "A", "width": 64, "height": 64})).unwrap();
        let undo = s.history.undo.len();
        for id in ["ai.generateImageLayer", "ai.voiceOver"] {
            assert_eq!(s.execute(id, json!({})).unwrap(), json!({"frontend": id}));
        }
        assert_eq!(s.history.undo.len(), undo);
        let asked: Vec<String> = s
            .drain_events()
            .into_iter()
            .filter_map(|e| match e {
                crate::Event::Frontend { command, .. } => Some(command),
                _ => None,
            })
            .collect();
        assert_eq!(asked, ["ai.settings", "ai.generateImageLayer", "ai.voiceOver"]);
    }
}
