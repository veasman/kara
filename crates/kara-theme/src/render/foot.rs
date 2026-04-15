use crate::{ResolvedTheme, UiMode};

/// Which INI section foot owns for the current mode.
///
/// We use mode-scoped `[colors-dark]` / `[colors-light]` rather
/// than the plain `[colors]` fallback because plain `[colors]` is
/// deprecated in foot and produces a startup warning on every foot
/// invocation.
///
/// To keep foot's runtime section dispatch unambiguous (so a
/// SIGUSR1 reload always knows which section to apply), we ALSO
/// patch `[main] theme=dark|light` alongside the color section
/// — see `foot_main_section_pairs`. With an explicit `theme=`
/// set, foot ignores the system color-scheme and always applies
/// the corresponding `[colors-<mode>]` section regardless of how
/// or when it reloads.
pub fn foot_color_section(theme: &ResolvedTheme) -> &'static str {
    match theme.mode {
        UiMode::Light => "colors-light",
        UiMode::Dark | UiMode::Auto => "colors-dark",
    }
}

/// Keys patched into foot's `[main]` section. Right now we only
/// set `theme=dark|light` so foot's mode selection is explicit —
/// without it, foot's dark/light auto-detect may not re-evaluate
/// on SIGUSR1 reload, and `[colors-dark]` changes would land in
/// the file but not take effect in the running display until
/// the user toggled system mode.
///
/// Returns pairs to be applied with `ini_patch::patch_ini_section`
/// against the `[main]` section. Note that foot treats top-of-file
/// keys as implicit `[main]` members — the patcher will either
/// create an explicit `[main]` header if needed, or merge into an
/// existing one.
pub fn foot_main_section_pairs(theme: &ResolvedTheme) -> Vec<(&'static str, String)> {
    let theme_value = match theme.mode {
        UiMode::Light => "light",
        UiMode::Dark | UiMode::Auto => "dark",
    };
    vec![("theme", theme_value.to_string())]
}

/// Return every color key kara-beautify owns as `(key, value)`
/// pairs. The `[colors-dark]` / `[colors-light]` section of the
/// user's foot.ini gets these keys patched in via
/// `ini_patch::patch_ini_section`.
///
/// ## Why patch-in-place, not an include file
///
/// Foot's `include=` directive is honored at server startup but is
/// NOT re-read on SIGUSR1 reload — meaning a generated include file
/// works once and then goes stale on every theme change until the
/// user restarts the foot server. Patching foot.ini directly works
/// with foot's native SIGUSR1 reload path, which DOES re-parse the
/// main config file. Same trade-off we accepted for GTK settings.ini:
/// kara owns a small, well-defined set of keys inside a user-owned
/// file, and the user's other keys (font, scrollback, cursor, csd)
/// are preserved by the INI patcher.
///
/// The ANSI palette uses `theme.ansi[0..16]` — the same 16-color
/// lookup kitty/tmux consume — so color families stay aligned
/// across terminals.
pub fn foot_color_pairs(theme: &ResolvedTheme) -> Vec<(&'static str, String)> {
    let c = &theme.semantic;
    let strip = |hex: String| hex.trim_start_matches('#').to_string();

    vec![
        ("alpha", format!("{:.2}", theme.style.opacity)),
        ("foreground", strip(c.fg0.to_hex())),
        ("background", strip(c.bg0.to_hex())),
        ("selection-foreground", strip(c.selection_fg.to_hex())),
        ("selection-background", strip(c.selection_bg.to_hex())),
        ("urls", strip(c.accent.to_hex())),
        ("regular0", strip(theme.ansi[0].to_hex())),
        ("regular1", strip(theme.ansi[1].to_hex())),
        ("regular2", strip(theme.ansi[2].to_hex())),
        ("regular3", strip(theme.ansi[3].to_hex())),
        ("regular4", strip(theme.ansi[4].to_hex())),
        ("regular5", strip(theme.ansi[5].to_hex())),
        ("regular6", strip(theme.ansi[6].to_hex())),
        ("regular7", strip(theme.ansi[7].to_hex())),
        ("bright0", strip(theme.ansi[8].to_hex())),
        ("bright1", strip(theme.ansi[9].to_hex())),
        ("bright2", strip(theme.ansi[10].to_hex())),
        ("bright3", strip(theme.ansi[11].to_hex())),
        ("bright4", strip(theme.ansi[12].to_hex())),
        ("bright5", strip(theme.ansi[13].to_hex())),
        ("bright6", strip(theme.ansi[14].to_hex())),
        ("bright7", strip(theme.ansi[15].to_hex())),
    ]
}

/// Human-readable dump of the section kara-beautify will patch
/// into foot.ini. Used by `kara-beautify render <theme> foot` for
/// dry-inspection — NOT used on the apply path, which goes
/// through `foot_color_pairs` + `ini_patch::patch_ini_section`.
pub fn render_foot_theme(theme: &ResolvedTheme) -> String {
    let section = foot_color_section(theme);
    let mut out = format!(
        "# kara-beautify would patch the following keys into the\n\
         # [{section}] section of ~/.config/foot/foot.ini:\n\n\
         [{section}]\n"
    );
    for (k, v) in foot_color_pairs(theme) {
        out.push_str(&format!("{k}={v}\n"));
    }
    out
}
