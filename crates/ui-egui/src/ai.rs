//! Layer ▸ AI through publik API or the person's own key, desktop only. The setup flow, the
//! credential file and the HTTP calls live in the shared `craft-publik` crate (one per Craft app).
//! This module is EffectCraft's thin host:
//!
//! - the setup dialog on the first launch (once, a few frames in, when no other dialog shows;
//!   "Later" or Escape ends it for good),
//! - Settings ▸ AI (status, sign-in and key controls, the setup flow in place),
//! - Layer ▸ AI ▸ Generate Image Layer… and Voice-over from Text…, two dialogs of the app's dialog
//!   system (`Dialog::AiImage`, `Dialog::AiVoice`), opened by the engine commands
//!   `ai.generateImageLayer` and `ai.voiceOver`.
//!
//! Nothing is sent, and nothing is charged, until the person clicks Generate. The answer (a PNG
//! from `client::image_generate`, an MP3 from `client::speech`) is saved in an "AI Media" folder
//! next to the saved project (in EffectCraft's settings folder while the project has no file) and
//! goes in through the app's own commands: `file.import` makes the footage item and
//! `layer.addItem` adds the layer to the composition that was active at Generate, at the time that
//! was current then (an image in the centre), both in one `engine.batch`, so one Edit ▸ Undo
//! removes it. A paid result that cannot go in at once waits behind Place; it is never dropped
//! unless the person clicks Discard.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError};

pub use craft_publik::AppInfo;
use craft_publik::client::{self, Fail, Reply};
use craft_publik::ui::{Action, AiState, spawn};
use craft_publik::{Line, Provider, WHY_IT_COSTS};
use effectcraft_engine::Services;
use effectcraft_engine::project::ItemId;
use effectcraft_engine::time::Tick;
use egui::{Color32, RichText, vec2};
use serde_json::json;

use crate::theme::Tokens;
use crate::{Dialog, EffectcraftApp};

/// The Settings (Preferences) page of the AI section.
pub const SETTINGS_PAGE: &str = "ai";
/// The folder generated media is kept in.
pub const MEDIA_FOLDER: &str = "AI Media";

/// The longest prompt the image dialog takes, in characters.
const MAX_PROMPT_CHARS: usize = 4_000;
/// The longest text the speech line reads in one request, in characters.
const MAX_SPEECH_CHARS: usize = 4_096;
/// The longest file name made from a prompt or a text, in characters.
const MAX_NAME_CHARS: usize = 40;

/// Generate Image Layer's shapes: (id, label, the image line's `size`).
const SHAPES: [(&str, &str, &str); 3] = [("square", "Square", "1024x1024"), ("landscape", "Landscape", "1536x1024"), ("portrait", "Portrait", "1024x1536")];
/// The speech line's voices: (the `voice` parameter, label).
const VOICES: [(&str, &str); 6] = [("alloy", "Alloy"), ("echo", "Echo"), ("fable", "Fable"), ("onyx", "Onyx"), ("nova", "Nova"), ("shimmer", "Shimmer")];

const OFF: &str = "AI is off in this session.";
const BUSY: &str = "Generating. This can take a minute.";
const STOPPED: &str = "The request stopped before an answer came back. Try again.";

/// One of the two features.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Image,
    Voice,
}

impl Kind {
    /// The dialog title, the menu entry and the undo step's name.
    fn title(self) -> &'static str {
        match self {
            Kind::Image => "Generate Image Layer",
            Kind::Voice => "Voice-over from Text",
        }
    }
    fn what(self) -> &'static str {
        match self {
            Kind::Image => "image",
            Kind::Voice => "voice-over",
        }
    }
    fn ext(self) -> &'static str {
        match self {
            Kind::Image => "png",
            Kind::Voice => "mp3",
        }
    }
    fn fallback_name(self) -> &'static str {
        match self {
            Kind::Image => "AI Image",
            Kind::Voice => "Voice-over",
        }
    }
    fn dialog(self) -> Dialog {
        match self {
            Kind::Image => Dialog::AiImage,
            Kind::Voice => Dialog::AiVoice,
        }
    }
    /// The automation id prefix of its dialog.
    fn auto(self) -> &'static str {
        match self {
            Kind::Image => "ai.image",
            Kind::Voice => "ai.voice",
        }
    }
    fn line(self) -> Line {
        match self {
            Kind::Image => Line::Images,
            Kind::Voice => Line::Speech,
        }
    }
}

/// Where a result goes: the composition that was active at Generate and its current time then.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Target {
    comp: ItemId,
    time: Tick,
}

/// A paid result: its bytes, the file name it gets, the file once written, and where it goes.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Out {
    kind: Kind,
    bytes: Vec<u8>,
    name: String,
    path: Option<String>,
    target: Option<Target>,
}

/// One feature's request in flight, its result waiting to be placed, and what its dialog says.
#[derive(Default)]
struct Task {
    rx: Option<Receiver<Result<Reply, Fail>>>,
    /// The file name and the target of the request in flight.
    pending: Option<(String, Option<Target>)>,
    waiting: Option<Out>,
    /// The prompt or the text to speak, kept while the app runs.
    text: String,
    /// The shape or the voice id.
    choice: String,
    note: Option<String>,
    error: Option<String>,
}

impl Task {
    fn busy(&self) -> bool {
        self.rx.is_some()
    }
}

/// AI state of the desktop app (`EffectcraftApp::ai`).
pub struct Ai {
    pub state: AiState,
    frames: u32,
    first_run_checked: bool,
    open_setup: bool,
    /// The setup dialog was open last frame.
    setup_shown: bool,
    /// The feature dialog to show again when the setup dialog ends.
    return_to: Option<Dialog>,
    /// Settings ▸ AI shows the setup flow in place of its status.
    inline_setup: bool,
    image: Task,
    voice: Task,
}

impl Ai {
    pub fn new(app: AppInfo) -> Self {
        Ai {
            state: AiState::new(app),
            frames: 0,
            first_run_checked: false,
            open_setup: false,
            setup_shown: false,
            return_to: None,
            inline_setup: false,
            image: Task { choice: SHAPES[0].0.into(), ..Task::default() },
            voice: Task { choice: VOICES[0].0.into(), ..Task::default() },
        }
    }

    /// Do not open the setup dialog on this launch (an agent drives the app over `--control`).
    /// Settings ▸ AI and the Layer ▸ AI dialogs still offer it.
    pub fn skip_first_run(&mut self) {
        self.first_run_checked = true;
    }

    /// A request is on its way.
    pub fn generating(&self) -> bool {
        self.image.busy() || self.voice.busy()
    }

    fn task(&self, kind: Kind) -> &Task {
        match kind {
            Kind::Image => &self.image,
            Kind::Voice => &self.voice,
        }
    }
    fn task_mut(&mut self, kind: Kind) -> &mut Task {
        match kind {
            Kind::Image => &mut self.image,
            Kind::Voice => &mut self.voice,
        }
    }

    /// The first-run ask ends for good, as "Later" ends it.
    fn end_first_run(&mut self) {
        if self.state.settings.setup_done || self.state.provider().is_set_up() {
            return;
        }
        self.state.settings.setup_done = true;
        if let Err(e) = self.state.settings.save(self.state.app.settings_path().as_deref()) {
            log::warn!("AI settings: {e}");
        }
    }
}

/// `PUBLIK_SELF_TEST=1`: provision for real and print one line (publikhq.com/skills/setup.md).
pub fn self_test(app: &AppInfo) -> String {
    craft_publik::self_test(app)
}

// ---- once a frame -------------------------------------------------------------------------------

/// Once a frame, before the dialogs draw: take finished requests (and place what they made), and
/// open the setup dialog on the first launch once no other dialog shows.
pub fn frame(app: &mut EffectcraftApp, ctx: &egui::Context) {
    let dialog = app.dialog;
    let quiet = dialog.is_none() && app.recovery.is_none();
    let Some(ai) = app.ai.as_mut() else { return };
    ai.state.poll();
    // The setup dialog went away without its own buttons (Escape): that ends the ask too.
    if ai.setup_shown && dialog != Some(Dialog::AiSetup) {
        ai.setup_shown = false;
        ai.return_to = None;
        ai.end_first_run();
    }
    let mut arrived = vec![];
    for kind in [Kind::Image, Kind::Voice] {
        if let Some(r) = take(ai, kind) {
            arrived.push((kind, r));
        }
    }
    ai.frames = ai.frames.saturating_add(1);
    if !ai.first_run_checked && ai.frames > 3 {
        ai.first_run_checked = true;
        ai.open_setup = ai.state.wants_first_run() || ai.state.owes_first_card();
    }
    let open_setup = ai.open_setup && quiet;
    for (kind, out) in arrived {
        arrived_out(app, ctx, kind, out);
    }
    if open_setup {
        open_setup_dialog(app, None);
    }
}

/// A finished request of `kind`, if one came in: its charge noted, its failure shown.
fn take(ai: &mut Ai, kind: Kind) -> Option<Out> {
    let task = ai.task_mut(kind);
    let r = match task.rx.as_ref()?.try_recv() {
        Ok(r) => r,
        Err(TryRecvError::Empty) => return None,
        Err(TryRecvError::Disconnected) => Err(Fail { kind: "local".into(), message: STOPPED.into(), ..Fail::default() }),
    };
    task.rx = None;
    let (name, target) = task.pending.take().unwrap_or_else(|| (kind.fallback_name().to_string(), None));
    match r {
        Ok(reply) if !reply.bytes.is_empty() => {
            ai.state.note_reply(&reply);
            Some(Out { kind, bytes: reply.bytes, name, path: None, target })
        }
        Ok(reply) => {
            ai.state.note_reply(&reply);
            ai.task_mut(kind).error = Some(format!("The service sent back no {}. Try again.", kind.what()));
            None
        }
        Err(f) => {
            ai.state.note_fail(&f);
            // A 402 or a retired key is the result bar's (one message, one link); the rest is ours.
            let error = ai.state.notice.is_none().then(|| f.message.clone());
            ai.task_mut(kind).error = error;
            None
        }
    }
}

/// A paid result came in: it goes in at once when it can, else it waits behind Place.
fn arrived_out(app: &mut EffectcraftApp, ctx: &egui::Context, kind: Kind, mut out: Out) {
    let target = out.target;
    let placed = place(app, ctx, &mut out, target);
    let dialog_open = app.dialog == Some(kind.dialog());
    let now = ctx.input(|i| i.time);
    let toast = match placed {
        Ok(note) => {
            if let Some(ai) = app.ai.as_mut() {
                ai.task_mut(kind).note = Some(note.clone());
            }
            note
        }
        Err(why) => {
            let note = waiting_note(&out, &why);
            if let Some(ai) = app.ai.as_mut() {
                let task = ai.task_mut(kind);
                task.note = Some(note);
                task.waiting = Some(out);
            }
            format!("The {} is ready. Open Layer ▸ AI ▸ {} and click Place.", kind.what(), kind.title())
        }
    };
    if !dialog_open {
        app.toast = Some((toast, now));
    }
}

/// "The image is ready, but it is not in the composition yet. <why>. <the file>. <what to do>"
fn waiting_note(out: &Out, why: &str) -> String {
    let mut s = format!("The {} is ready, but it is not in a composition yet.", out.kind.what());
    let why = why.trim().trim_end_matches('.');
    if !why.is_empty() {
        s.push(' ');
        s.push_str(why);
        s.push('.');
    }
    if let Some(p) = &out.path {
        s.push_str(&format!(" The file is saved at {p}."));
    }
    s.push_str(" Open a composition, then click Place.");
    s
}

// ---- placing a result ---------------------------------------------------------------------------

/// The folder generated media goes in: [`MEDIA_FOLDER`] next to the saved project, else in
/// EffectCraft's settings folder (an unsaved project has no folder of its own).
fn media_dir(project: Option<&str>, config_dir: Option<&Path>) -> Option<PathBuf> {
    let beside_project = project.and_then(|p| Path::new(p).parent()).filter(|d| !d.as_os_str().is_empty());
    beside_project.or(config_dir).map(|d| d.join(MEDIA_FOLDER))
}

/// A file name from the prompt or the text: its first words (letters, digits, spaces, `-` and
/// `_`), at most [`MAX_NAME_CHARS`] characters, or `fallback`.
fn file_stem(text: &str, fallback: &str) -> String {
    let cleaned: String = text.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { ' ' }).collect();
    let words = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    let stem: String = words.chars().take(MAX_NAME_CHARS).collect();
    let stem = stem.trim().to_string();
    if stem.is_empty() { fallback.to_string() } else { stem }
}

/// `dir/stem.ext`, or `dir/stem 2.ext`, `dir/stem 3.ext`… when that file exists.
fn free_path(services: &dyn Services, dir: &Path, stem: &str, ext: &str) -> String {
    let at = |name: String| dir.join(name).to_string_lossy().to_string();
    let first = at(format!("{stem}.{ext}"));
    if !services.exists(&first) {
        return first;
    }
    (2..10_000).map(|n| at(format!("{stem} {n}.{ext}"))).find(|p| !services.exists(p)).unwrap_or_else(|| at(format!("{stem} {}.{ext}", std::process::id())))
}

/// Save the result's bytes once (they stay on disk if the layer cannot be added). Returns the file.
fn write_out(app: &EffectcraftApp, out: &mut Out) -> Result<String, String> {
    if let Some(p) = &out.path {
        return Ok(p.clone());
    }
    let config = app.ai.as_ref().and_then(|ai| ai.state.app.config_dir.clone());
    let dir = media_dir(app.session.path.as_deref(), config.as_deref()).ok_or("EffectCraft has no folder to save the file in")?;
    let path = free_path(app.session.services.as_ref(), &dir, &out.name, out.kind.ext());
    app.session.services.store_file(&path, &out.bytes).map_err(|e| format!("EffectCraft could not save the file ({e})"))?;
    out.path = Some(path.clone());
    Ok(path)
}

/// Put a result into `target` through the app's own commands: `file.import` makes the footage
/// item and `layer.addItem` adds it as a layer at the target's time (centred), in one undo step.
/// Returns the note for the dialog, or why it cannot go in now.
fn place(app: &mut EffectcraftApp, ctx: &egui::Context, out: &mut Out, target: Option<Target>) -> Result<String, String> {
    let target = target.ok_or("No composition is open")?;
    let project = &app.session.project;
    let comp_name = project.comp(target.comp).and(project.item(target.comp)).map(|i| i.name.clone()).ok_or("The composition it was made for is gone")?;
    if app.session.state.text_edit.is_some() {
        return Err("Text editing is in progress. Finish it first".into());
    }
    let path = write_out(app, out)?;
    let params = json!({
        "label": out.kind.title(),
        "steps": [
            // One still, never a numbered run ("Sky 2.png" next to "Sky 3.png").
            {"command": "file.import", "params": {"paths": [path], "sequence": false}},
            {"command": "layer.addItem", "params": {"comp": target.comp.0, "item": "$1.items.0", "time": target.time.seconds()}},
        ],
    });
    let r = crate::menus::run_engine(app, ctx, "engine.batch", params).map_err(|e| format!("EffectCraft could not add it to the composition ({e})"))?;
    let layer = r.pointer("/results/1/layer").and_then(serde_json::Value::as_u64);
    let name = app
        .session
        .project
        .comp(target.comp)
        .and_then(|c| c.layers.iter().find(|l| Some(l.id.0) == layer))
        .map_or_else(|| out.name.clone(), |l| l.name.clone());
    let at = app.session.project.comp(target.comp).map(|c| crate::panels::timecode(&app.session, c, target.time)).unwrap_or_default();
    Ok(format!("The {} is in {comp_name} as a new layer, \u{201c}{name}\u{201d}, at {at}. Edit ▸ Undo removes it.", out.kind.what()))
}

/// Place: the waiting result goes into the active composition at its current time.
fn place_waiting(app: &mut EffectcraftApp, ctx: &egui::Context, kind: Kind) {
    let Some(mut out) = app.ai.as_mut().and_then(|ai| ai.task_mut(kind).waiting.take()) else { return };
    let target = app.session.active_comp_id().map(|comp| Target { comp, time: app.session.time_of(comp) });
    let placed = place(app, ctx, &mut out, target);
    let Some(ai) = app.ai.as_mut() else { return };
    let task = ai.task_mut(kind);
    match placed {
        Ok(note) => {
            task.note = Some(note);
            task.error = None;
        }
        Err(why) => {
            task.note = Some(waiting_note(&out, &why));
            task.waiting = Some(out);
        }
    }
}

// ---- requests -----------------------------------------------------------------------------------

/// Generate: send the dialog's prompt or text. The only place a request starts.
fn start(app: &mut EffectcraftApp, ctx: &egui::Context, kind: Kind) -> Result<(), String> {
    let target = app.session.active_comp_id().map(|comp| Target { comp, time: app.session.time_of(comp) }).ok_or("Open a composition first")?;
    let ai = app.ai.as_mut().ok_or(OFF)?;
    let provider = ai.state.provider();
    if !provider.supports(kind.line()) {
        return Err(Fail::unsupported(kind.line()).message);
    }
    let task = ai.task_mut(kind);
    if task.busy() || task.waiting.is_some() {
        return Ok(());
    }
    let text = task.text.trim().to_string();
    if text.is_empty() {
        return Err(match kind {
            Kind::Image => "Describe the image first.".into(),
            Kind::Voice => "Type the text to speak first.".into(),
        });
    }
    task.pending = Some((file_stem(&text, kind.fallback_name()), Some(target)));
    task.note = None;
    task.error = None;
    let choice = task.choice.clone();
    task.rx = Some(match kind {
        Kind::Image => {
            let size = SHAPES.iter().find(|s| s.0 == choice).map_or(SHAPES[0].2, |s| s.2);
            spawn(ctx, move || client::image_generate(&provider, &text, size))
        }
        Kind::Voice => {
            let voice = VOICES.iter().find(|v| v.0 == choice).map_or(VOICES[0].0, |v| v.0);
            spawn(ctx, move || client::speech(&provider, &text, voice))
        }
    });
    Ok(())
}

// ---- opening ------------------------------------------------------------------------------------

/// The frontend half of the `ai.*` commands: open their dialogs or Settings ▸ AI.
pub fn command(app: &mut EffectcraftApp, id: &str) -> Result<serde_json::Value, String> {
    if app.ai.is_none() {
        return Err(OFF.into());
    }
    match id {
        "ai.generateImageLayer" => app.dialog = Some(Dialog::AiImage),
        "ai.voiceOver" => app.dialog = Some(Dialog::AiVoice),
        "ai.settings" => crate::panels::settings::open(app, SETTINGS_PAGE),
        _ => return Err(format!("unknown AI command `{id}`")),
    }
    Ok(json!({"dialog": id}))
}

/// The setup flow in a dialog of its own; `back` comes back when it ends.
fn open_setup_dialog(app: &mut EffectcraftApp, back: Option<Dialog>) {
    let Some(ai) = app.ai.as_mut() else { return };
    ai.open_setup = false;
    ai.state.begin();
    ai.setup_shown = true;
    ai.return_to = back;
    app.dialog = Some(Dialog::AiSetup);
}

/// Whether a Layer ▸ AI entry can run: AI is on here, and the command's own state allows it.
pub fn enabled(app: &EffectcraftApp, command: &str) -> bool {
    app.ai.is_some() && app.session.is_enabled(command)
}

fn open_url(ctx: &egui::Context, action: Action) {
    if let Action::OpenUrl(url) = action {
        ctx.open_url(egui::OpenUrl::new_tab(url));
    }
}

// ---- the dialogs --------------------------------------------------------------------------------

/// Draw the AI dialog `d` (`Dialog::AiSetup`, `AiImage` or `AiVoice`).
pub fn dialog(app: &mut EffectcraftApp, ctx: &egui::Context, t: &Tokens, d: Dialog) {
    match d {
        Dialog::AiSetup => setup_dialog(app, ctx, t),
        Dialog::AiImage => feature_dialog(app, ctx, t, Kind::Image),
        Dialog::AiVoice => feature_dialog(app, ctx, t, Kind::Voice),
        _ => {}
    }
}

fn setup_dialog(app: &mut EffectcraftApp, ctx: &egui::Context, t: &Tokens) {
    let mut action = Action::None;
    crate::panels::dialogs::modal(ctx, "AI", vec2(520.0, 420.0), t, |ui| match app.ai.as_mut() {
        Some(ai) => action = ai.state.setup_ui(ui),
        None => {
            ui.label(RichText::new(OFF).color(t.text_dim));
            if ui.button("Close").clicked() {
                action = Action::Close;
            }
        }
    });
    match action {
        Action::None => {}
        Action::OpenUrl(_) => open_url(ctx, action),
        Action::Close => {
            let back = app.ai.as_mut().and_then(|ai| {
                ai.setup_shown = false;
                ai.return_to.take()
            });
            app.dialog = back;
        }
    }
}

/// The cost sentence shown before the button that sends: publik's (from the credential, or the
/// crate's own), or who bills an own key.
fn cost_line(state: &AiState, provider: &Provider) -> String {
    match provider {
        Provider::Publik { .. } => state.credential.as_ref().map(|c| c.cost_sentence.trim()).filter(|s| !s.is_empty()).unwrap_or(WHY_IT_COSTS).to_string(),
        Provider::Own { service, .. } => format!("Your own key's service ({}) bills this request, not publik.", service.name()),
        Provider::NotSetUp => String::new(),
    }
}

/// A dialog's default button: the accent fill while it can be clicked, a plain button when not.
fn primary(label: &str, enabled: bool, t: &Tokens) -> egui::Button<'static> {
    if enabled { egui::Button::new(RichText::new(label.to_string()).color(Color32::WHITE)).fill(t.accent) } else { egui::Button::new(label.to_string()) }
}

/// What the person clicked in a feature dialog.
enum Click {
    Generate,
    Place,
    Discard,
    SetUp,
    Close,
}

fn feature_dialog(app: &mut EffectcraftApp, ctx: &egui::Context, t: &Tokens, kind: Kind) {
    let target = app.session.active_comp().map(|c| {
        let name = app.session.active_comp_id().and_then(|id| app.session.project.item(id)).map(|i| i.name.clone()).unwrap_or_default();
        (name, crate::panels::timecode(&app.session, c, app.session.time()))
    });
    let mut click: Option<Click> = None;
    let mut action = Action::None;
    let mut autos: Vec<(String, egui::Rect, &'static str)> = vec![];
    let p = kind.auto();
    crate::panels::dialogs::modal(ctx, kind.title(), vec2(560.0, 400.0), t, |ui| {
        let Some(ai) = app.ai.as_mut() else {
            ui.label(RichText::new(OFF).color(t.text_dim));
            ui.add_space(10.0);
            if ui.button("Close").clicked() {
                click = Some(Click::Close);
            }
            return;
        };
        let provider = ai.state.provider();
        if !provider.is_set_up() {
            ui.add(egui::Label::new(format!("{} needs AI. Set it up first: publik API (no key to paste) or your own key.", kind.title())).wrap());
            ui.add_space(12.0);
            ui.allocate_ui_with_layout(vec2(ui.available_width(), 26.0), egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let r = ui.add(primary("Set Up AI...", true, t));
                autos.push((format!("{p}.setUp"), r.rect, "Set Up AI"));
                if r.clicked() {
                    click = Some(Click::SetUp);
                }
                let r = ui.button("Close");
                autos.push((format!("{p}.close"), r.rect, "Close"));
                if r.clicked() {
                    click = Some(Click::Close);
                }
            });
            return;
        }
        let supported = provider.supports(kind.line());
        let task = ai.task_mut(kind);
        let (busy, waiting) = (task.busy(), task.waiting.is_some());
        let intro = match kind {
            Kind::Image => "Describe an image. EffectCraft adds it to the active composition as a new layer, at the current time, in the centre.",
            Kind::Voice => {
                "Type the text to speak and pick a voice. EffectCraft adds the speech to the active composition as a new audio layer, at the current time."
            }
        };
        ui.add(egui::Label::new(intro).wrap());
        ui.add_space(2.0);
        let place_line = match &target {
            Some((comp, at)) => format!("It goes into {comp} at {at}. Nothing is sent until you click Generate."),
            None => "No composition is open. Open one first. Nothing is sent until you click Generate.".to_string(),
        };
        ui.add(egui::Label::new(RichText::new(place_line).size(11.0).color(t.text_dim)).wrap());
        ui.add_space(8.0);
        let (label, hint, limit) = match kind {
            Kind::Image => ("Describe the image", "A lighthouse on a cliff at dawn, soft light", MAX_PROMPT_CHARS),
            Kind::Voice => ("Text to speak", "Welcome to the show. Here is what is new this week.", MAX_SPEECH_CHARS),
        };
        ui.label(RichText::new(label).color(t.text));
        let edit = egui::TextEdit::multiline(&mut task.text)
            .id(egui::Id::new(("ai-text", p)))
            .hint_text(hint)
            .desired_rows(4)
            .desired_width(f32::INFINITY)
            .char_limit(limit);
        let r = ui.add_enabled(!busy && !waiting, edit);
        crate::widgets::focus_on_open(ui.ctx(), &r, &task.text);
        autos.push((format!("{p}.text"), r.rect, label));
        ui.add_space(8.0);
        ui.add_enabled_ui(!busy && !waiting, |ui| match kind {
            Kind::Image => {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Shape").color(t.text));
                    for (id, shown, _) in SHAPES {
                        let r = ui.radio(task.choice == id, shown);
                        autos.push((format!("{p}.shape.{id}"), r.rect, shown));
                        if r.clicked() {
                            task.choice = id.to_string();
                        }
                    }
                });
            }
            Kind::Voice => {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Voice").color(t.text));
                    let shown = VOICES.iter().find(|v| v.0 == task.choice).map_or(VOICES[0].1, |v| v.1);
                    let r = egui::ComboBox::from_id_salt(("ai-voice", p)).selected_text(shown).width(160.0).show_ui(ui, |ui| {
                        for (id, name) in VOICES {
                            if ui.selectable_label(task.choice == id, name).clicked() {
                                task.choice = id.to_string();
                            }
                        }
                    });
                    autos.push((format!("{p}.voice"), r.response.rect, "Voice"));
                });
            }
        });
        ui.add_space(8.0);
        if supported {
            let cost = cost_line(&ai.state, &provider);
            if !cost.is_empty() {
                ui.add(egui::Label::new(RichText::new(cost).size(11.0).color(t.text_dim)).wrap());
            }
        } else {
            ui.add(egui::Label::new(RichText::new(Fail::unsupported(kind.line()).message).color(t.warning)).wrap());
        }
        ui.add_space(10.0);
        let task = ai.task(kind);
        let can_generate = supported && !busy && target.is_some() && !task.text.trim().is_empty();
        ui.allocate_ui_with_layout(vec2(ui.available_width(), 26.0), egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if waiting {
                let r = ui.add_enabled(target.is_some(), primary("Place", target.is_some(), t));
                autos.push((format!("{p}.place"), r.rect, "Place"));
                if r.clicked() {
                    click = Some(Click::Place);
                }
            } else {
                let r = ui.add_enabled(can_generate, primary("Generate", can_generate, t));
                autos.push((format!("{p}.generate"), r.rect, "Generate"));
                if r.clicked() {
                    click = Some(Click::Generate);
                }
            }
            let r = ui.button("Close");
            autos.push((format!("{p}.close"), r.rect, "Close"));
            if r.clicked() {
                click = Some(Click::Close);
            }
            if waiting {
                let r = ui.button("Discard");
                autos.push((format!("{p}.discard"), r.rect, "Discard"));
                if r.clicked() {
                    click = Some(Click::Discard);
                }
            }
        });
        if busy {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new(BUSY).color(t.text_dim));
            });
        }
        if let Some(note) = &task.note {
            ui.add_space(6.0);
            ui.add(egui::Label::new(RichText::new(note).color(t.text)).wrap());
        }
        if let Some(error) = &task.error {
            ui.add_space(6.0);
            ui.add(egui::Label::new(RichText::new(error).color(t.danger)).wrap());
        }
        ui.add_space(6.0);
        action = ai.state.result_bar(ui);
        ui.label(RichText::new(ai.state.status_line()).size(11.0).color(t.text_faint));
    });
    for (id, rect, label) in autos {
        app.auto.add(&id, rect, label);
    }
    open_url(ctx, action);
    match click {
        None => {}
        Some(Click::Close) => app.dialog = None,
        Some(Click::SetUp) => open_setup_dialog(app, Some(kind.dialog())),
        Some(Click::Generate) => {
            if let Err(e) = start(app, ctx, kind)
                && let Some(ai) = app.ai.as_mut()
            {
                ai.task_mut(kind).error = Some(e);
            }
        }
        Some(Click::Place) => place_waiting(app, ctx, kind),
        Some(Click::Discard) => {
            if let Some(ai) = app.ai.as_mut() {
                let task = ai.task_mut(kind);
                task.waiting = None;
                task.note = None;
            }
        }
    }
}

// ---- Settings ▸ AI ------------------------------------------------------------------------------

/// The AI page of the Settings dialog (desktop, where AI is on).
pub fn settings_page() -> effectcraft_engine::prefs::Page {
    effectcraft_engine::prefs::Page { id: SETTINGS_PAGE, title: "AI", items: vec![] }
}

/// Settings ▸ AI: what is in use, the sign-in and key controls, and the setup flow in place. AI
/// settings take effect at once (OK and Cancel do not apply to them).
pub fn settings_ui(app: &mut EffectcraftApp, ui: &mut egui::Ui) {
    let t = app.tokens;
    let ctx = ui.ctx().clone();
    let Some(ai) = app.ai.as_mut() else {
        ui.label(RichText::new(OFF).color(t.text_dim));
        return;
    };
    ui.add(
        egui::Label::new(
            RichText::new("Layer ▸ AI ▸ Generate Image Layer and Voice-over from Text use publik API or your own key. Changes here take effect at once.")
                .color(t.text_dim),
        )
        .wrap(),
    );
    ui.add_space(6.0);
    let action = if ai.inline_setup {
        let action = ai.state.setup_ui(ui);
        if action == Action::Close {
            ai.inline_setup = false;
        }
        action
    } else {
        let mut open_setup = false;
        let action = ai.state.options_ui(ui, &mut open_setup);
        ai.inline_setup = open_setup;
        action
    };
    open_url(&ctx, action);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn temp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("effectcraft-ai-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn info(dir: &Path) -> AppInfo {
        AppInfo {
            slug: "effectcraft-test",
            name: "EffectCraft",
            version: "0.0.0",
            app_token: "",
            dialects: &["images", "audio_speech"],
            config_dir: Some(dir.to_path_buf()),
        }
    }

    /// An app with a 320×180 composition at 1 s and AI on (its settings in `dir`).
    fn app_at_one_second(dir: &Path) -> (EffectcraftApp, ItemId) {
        let mut s = effectcraft_host::session();
        s.execute("comp.new", json!({"name": "Main", "width": 320, "height": 180, "duration": 4})).unwrap();
        s.execute("time.set", json!({"time": 1.0})).unwrap();
        let cid = s.active_comp_id().unwrap();
        let mut app = EffectcraftApp::new(s);
        app.ai = Some(Ai::new(info(dir)));
        (app, cid)
    }

    fn png(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbaImage::from_pixel(w, h, image::Rgba([200, 40, 40, 255]));
        let mut out = std::io::Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }

    /// MPEG-1 Layer III frames of silence (128 kb/s, 44.1 kHz, mono): about a second of audio.
    fn mp3() -> Vec<u8> {
        let mut frame = vec![0u8; 417];
        frame[..4].copy_from_slice(&[0xFF, 0xFB, 0x90, 0xC4]);
        frame.repeat(40)
    }

    #[test]
    fn file_names_come_from_the_text() {
        assert_eq!(file_stem("A lighthouse, on a cliff: at dawn!", "AI Image"), "A lighthouse on a cliff at dawn");
        assert_eq!(file_stem("  ../../etc/passwd  ", "AI Image"), "etc passwd");
        assert_eq!(file_stem("?!", "Voice-over"), "Voice-over");
        assert_eq!(file_stem(&"word ".repeat(30), "x").chars().count(), MAX_NAME_CHARS - 1);
    }

    #[test]
    fn media_goes_beside_the_saved_project_or_in_the_settings_folder() {
        let config = Path::new("/config");
        assert_eq!(media_dir(Some("/work/show/edit.ecproj"), Some(config)), Some(Path::new("/work/show").join(MEDIA_FOLDER)));
        assert_eq!(media_dir(None, Some(config)), Some(config.join(MEDIA_FOLDER)));
        assert_eq!(media_dir(Some("edit.ecproj"), Some(config)), Some(config.join(MEDIA_FOLDER)));
        assert_eq!(media_dir(None, None), None);
    }

    /// A generated image goes in through File ▸ Import and Add Footage to Comp: a footage item
    /// and a layer at the time of the request, in the centre, as one undo step.
    #[test]
    fn a_generated_image_becomes_one_undoable_layer_at_the_time_in_the_centre() {
        let dir = temp("image");
        let (mut app, cid) = app_at_one_second(&dir);
        let ctx = egui::Context::default();
        let layers = app.session.project.comp(cid).unwrap().layers.len();
        let undo = app.session.history.undo.len();
        let target = Some(Target { comp: cid, time: app.session.time_of(cid) });
        // The person moves on while the request runs: the image still goes where it was asked for.
        app.session.execute("time.set", json!({"time": 3.0})).unwrap();
        let mut out = Out { kind: Kind::Image, bytes: png(64, 32), name: "Red square".into(), path: None, target };
        let note = place(&mut app, &ctx, &mut out, target).unwrap();
        assert!(note.contains("Main"), "{note}");
        let path = out.path.clone().unwrap();
        assert_eq!(Path::new(&path), dir.join(MEDIA_FOLDER).join("Red square.png"));
        assert!(Path::new(&path).is_file());
        let comp = app.session.project.comp(cid).unwrap();
        assert_eq!(comp.layers.len(), layers + 1);
        let layer = comp.layers.iter().find(|l| l.name.starts_with("Red square")).unwrap();
        assert_eq!(Some(layer.start_time), target.map(|t| t.time), "the time of the request (1 s, on a frame)");
        let pos = app.session.execute("prop.get", json!({"layer": layer.id.0, "path": "transform/position"})).unwrap()["value"].clone();
        assert_eq!((pos[0].as_f64(), pos[1].as_f64()), (Some(160.0), Some(90.0)), "{pos}");
        assert_eq!(app.session.history.undo.len(), undo + 1, "one undo step");
        assert_eq!(app.session.history.undo.last().map(|(l, _)| l.as_str()), Some("Generate Image Layer"));
        app.session.execute("edit.undo", json!({})).unwrap();
        assert_eq!(app.session.project.comp(cid).unwrap().layers.len(), layers);
        assert!(!app.session.project.items.values().any(|i| i.name.starts_with("Red square")), "the footage item went with it");
        // A second image of the same name keeps the first file.
        let mut again = Out { path: None, ..out.clone() };
        place(&mut app, &ctx, &mut again, target).unwrap();
        assert_eq!(Path::new(again.path.as_deref().unwrap()), dir.join(MEDIA_FOLDER).join("Red square 2.png"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_voice_over_becomes_an_audio_layer_at_the_time() {
        let dir = temp("voice");
        let (mut app, cid) = app_at_one_second(&dir);
        let ctx = egui::Context::default();
        let target = Some(Target { comp: cid, time: app.session.time_of(cid) });
        let mut out = Out { kind: Kind::Voice, bytes: mp3(), name: "Welcome".into(), path: None, target };
        place(&mut app, &ctx, &mut out, target).unwrap();
        let comp = app.session.project.comp(cid).unwrap();
        let layer = comp.layers.iter().find(|l| l.name.starts_with("Welcome")).unwrap();
        assert_eq!(Some(layer.start_time), target.map(|t| t.time));
        let effectcraft_engine::project::LayerSource::Footage { item } = layer.source else { panic!("{:?}", layer.source) };
        let Some(effectcraft_engine::project::ItemKind::Footage(f)) = app.session.project.item(item).map(|i| &i.kind) else { panic!("no footage") };
        assert!(f.has_audio && !f.has_video, "{f:?}");
        assert_eq!(app.session.history.undo.last().map(|(l, _)| l.as_str()), Some("Voice-over from Text"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A result whose composition is gone, or that cannot be read, waits behind Place: the file
    /// is kept, nothing half-done stays in the project, and Place puts it in the active comp.
    #[test]
    fn a_result_that_cannot_go_in_waits_and_place_puts_it_in() {
        let dir = temp("wait");
        let (mut app, cid) = app_at_one_second(&dir);
        let ctx = egui::Context::default();
        let items = app.session.project.items.len();
        let gone = Some(Target { comp: ItemId(999_999), time: Tick::ZERO });
        arrived_out(&mut app, &ctx, Kind::Image, Out { kind: Kind::Image, bytes: png(8, 8), name: "Later".into(), path: None, target: gone });
        let task = &app.ai.as_ref().unwrap().image;
        assert!(task.waiting.is_some());
        assert!(task.note.as_deref().unwrap_or("").contains("click Place"), "{:?}", task.note);
        assert!(app.toast.as_ref().is_some_and(|(t, _)| t.contains("Place")), "the dialog is closed, so a toast says where it waits");
        assert_eq!(app.session.project.items.len(), items);
        // Bytes that are not an image: the import fails, the batch rolls back, the result waits.
        let mut bad =
            Out { kind: Kind::Image, bytes: b"not a png".to_vec(), name: "Broken".into(), path: None, target: Some(Target { comp: cid, time: Tick::ZERO }) };
        let undo = app.session.history.undo.len();
        let bad_target = bad.target;
        assert!(place(&mut app, &ctx, &mut bad, bad_target).is_err());
        assert!(bad.path.as_deref().is_some_and(|p| Path::new(p).is_file()), "the paid bytes stay on disk");
        assert_eq!(app.session.project.items.len(), items);
        assert_eq!(app.session.history.undo.len(), undo);
        // Place: into the active composition at its current time.
        app.session.execute("time.set", json!({"time": 2.0})).unwrap();
        place_waiting(&mut app, &ctx, Kind::Image);
        let task = &app.ai.as_ref().unwrap().image;
        assert!(task.waiting.is_none(), "{:?}", task.note);
        let layer = app.session.project.comp(cid).unwrap().layers.iter().find(|l| l.name.starts_with("Later")).map(|l| l.start_time);
        assert_eq!(layer, Some(app.session.time_of(cid)));
        assert!(app.session.time_of(cid) > Tick::from_seconds_f64(1.9));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The dialogs send nothing on their own: opening them, typing and picking start no request.
    #[test]
    fn opening_a_dialog_sends_nothing() {
        let dir = temp("open");
        let (mut app, _) = app_at_one_second(&dir);
        for id in ["ai.generateImageLayer", "ai.voiceOver"] {
            let r = command(&mut app, id).unwrap();
            assert_eq!(r["dialog"], Value::from(id));
            assert!(!app.ai.as_ref().unwrap().generating());
        }
        assert!(command(&mut app, "ai.settings").is_ok());
        assert_eq!(app.dialog, Some(Dialog::Settings));
        assert_eq!(app.dialog_state.settings_page, SETTINGS_PAGE);
        // Generate with an empty prompt says what is missing and sends nothing.
        app.ai.as_mut().unwrap().state.settings.provider = String::new();
        let ctx = egui::Context::default();
        assert!(start(&mut app, &ctx, Kind::Image).is_err());
        assert!(!app.ai.as_ref().unwrap().generating());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Without AI (tests, scripts, no settings folder) the Layer ▸ AI entries are off.
    #[test]
    fn without_ai_the_entries_are_off() {
        let mut s = effectcraft_engine::Session::default();
        s.execute("comp.new", json!({"name": "A", "width": 64, "height": 64})).unwrap();
        let mut app = EffectcraftApp::new(s);
        assert!(!enabled(&app, "ai.generateImageLayer"));
        assert!(command(&mut app, "ai.voiceOver").is_err());
        let dir = temp("on");
        app.ai = Some(Ai::new(info(&dir)));
        assert!(enabled(&app, "ai.generateImageLayer"));
        assert!(enabled(&app, "ai.settings"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
