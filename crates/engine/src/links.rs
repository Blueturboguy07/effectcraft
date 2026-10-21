//! Community and project links (shown in Help ▸, the About dialog, the start screen and the
//! header).

pub const APP_NAME: &str = "EffectCraft";
pub const APP_SLUG: &str = "effectcraft";
pub const WEBSITE: &str = "https://publikhq.com/adobe";
pub const APP_PAGE: &str = "https://publikhq.com/effectcraft";
pub const GITHUB: &str = "https://github.com/Blueturboguy07/effectcraft";
pub const ISSUES: &str = "https://github.com/Blueturboguy07/effectcraft/issues";

/// Sibling apps of the ArtCraft family: (name, what it is, GitHub repo, app page).
pub const SIBLINGS: &[(&str, &str)] = &[
    ("PhotoCraft", "photocraft"),
    ("VectorCraft", "vectorcraft"),
    ("FilmCraft", "filmcraft"),
    ("LightCraft", "lightcraft"),
    ("PdfCraft", "pdfcraft"),
    ("DesignCraft", "designcraft"),
];

pub fn github_of(slug: &str) -> String {
    format!("https://github.com/Blueturboguy07/{slug}")
}
pub fn page_of(slug: &str) -> String {
    format!("https://publikhq.com/{slug}")
}
