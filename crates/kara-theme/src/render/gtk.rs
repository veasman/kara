use crate::ResolvedTheme;

/// Return the five GTK `[Settings]` keys kara-beautify owns as
/// `(key, value)` pairs. Callers are expected to PATCH these keys
/// into an existing `settings.ini` rather than replace the file —
/// users typically ship their own settings.ini with additional keys
/// (font-name, button order, etc.) and beautify must not clobber them.
pub fn gtk_settings_pairs(theme: &ResolvedTheme) -> Vec<(&'static str, String)> {
    vec![
        ("gtk-theme-name", theme.gtk_theme_name().to_string()),
        ("gtk-icon-theme-name", theme.gtk_icon_theme_name().to_string()),
        ("gtk-cursor-theme-name", theme.cursor.theme.clone()),
        (
            "gtk-cursor-theme-size",
            theme.cursor.size.to_string(),
        ),
        (
            "gtk-application-prefer-dark-theme",
            theme.prefer_dark_flag().to_string(),
        ),
    ]
}

/// Human-readable dump of the keys beautify would write. Used by the
/// `kara-beautify render <theme> gtk` CLI so you can see what's going
/// to land before running apply. Not used in the actual write path —
/// real applies go through `patch_gtk_settings_file`.
pub fn render_gtk_settings(theme: &ResolvedTheme) -> String {
    let mut out = String::from("# patched in place by kara-beautify\n[Settings]\n");
    for (k, v) in gtk_settings_pairs(theme) {
        out.push_str(&format!("{k}={v}\n"));
    }
    out
}
