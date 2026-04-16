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

// NOTE: earlier iterations patched a `[main] theme=dark|light`
// key to pin foot's mode dispatch, but `theme` is NOT a valid
// key in foot's [main] section — foot errors out on config parse
// with "not a valid option: theme" and falls back to whatever
// config it last successfully loaded (usually the startup one).
// That's what broke foot reload during the daemon debug thread.
//
// The correct model is: foot determines dark/light via
// xdg-desktop-portal's org.freedesktop.appearance color-scheme
// setting at runtime. We don't need to patch anything in [main] —
// the round-trip SIGUSR2→SIGUSR1 in reload_foot() is enough to
// force foot to re-read the active [colors-<mode>] section.

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

/// Build an OSC escape sequence string that reprograms every standard
/// terminal color in place. Foot (and any xterm-compatible terminal)
/// parses these at runtime — no config reload, no restart, no SIGUSR
/// round-trip. Writing this string to a running terminal's pts
/// immediately swaps its colors.
///
/// Sequences included:
///   * OSC 10 — default foreground
///   * OSC 11 — default background
///   * OSC 12 — cursor
///   * OSC 17 — selection background
///   * OSC 19 — selection foreground
///   * OSC 4;N — palette entries 0..15 (ANSI)
///
/// Each sequence is terminated with ST (`ESC \`), which foot accepts
/// without the terminal bell side-effect that BEL termination causes
/// in some shells. Sequences are concatenated into a single write so
/// the terminal applies the whole theme atomically.
pub fn render_foot_osc_sequences(theme: &ResolvedTheme) -> String {
    let c = &theme.semantic;
    let mut out = String::new();
    // Helper: OSC N ; <hex> ST. hex is "#rrggbb" — the leading '#' is
    // accepted by foot and most xterm-compatible parsers.
    fn osc(out: &mut String, code: u32, val: String) {
        out.push_str(&format!("\x1b]{code};{val}\x1b\\"));
    }
    osc(&mut out, 10, c.fg0.to_hex());         // fg
    osc(&mut out, 11, c.bg0.to_hex());         // bg
    osc(&mut out, 12, c.accent.to_hex());      // cursor
    osc(&mut out, 17, c.selection_bg.to_hex()); // selection bg
    osc(&mut out, 19, c.selection_fg.to_hex()); // selection fg
    for (i, color) in theme.ansi.iter().enumerate() {
        // OSC 4 ; <idx> ; <hex> ST
        out.push_str(&format!("\x1b]4;{};{}\x1b\\", i, color.to_hex()));
    }
    out
}
