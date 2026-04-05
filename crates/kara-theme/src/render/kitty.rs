use crate::ResolvedTheme;

pub fn render_kitty_theme(theme: &ResolvedTheme) -> String {
    let c = &theme.semantic;
    format!(
        "font_family {}
font_size {}

background_opacity {:.2}

cursor_shape block
cursor_blink_interval 0
cursor {}
cursor_text_color {}

selection_background {}
selection_foreground {}

foreground {}
background {}

active_border_color {}
inactive_border_color {}
bell_border_color {}

color0  {}
color1  {}
color2  {}
color3  {}
color4  {}
color5  {}
color6  {}
color7  {}

color8  {}
color9  {}
color10 {}
color11 {}
color12 {}
color13 {}
color14 {}
color15 {}
",
        theme.fonts.mono_family,
        theme.fonts.mono_size,
        theme.style.opacity,
        c.fg0.to_hex(),
        c.bg0.to_hex(),
        c.selection_bg.to_hex(),
        c.selection_fg.to_hex(),
        c.fg0.to_hex(),
        c.bg0.to_hex(),
        c.accent.to_hex(),
        c.border_subtle.to_hex(),
        c.warning.to_hex(),
        theme.ansi[0].to_hex(),
        theme.ansi[1].to_hex(),
        theme.ansi[2].to_hex(),
        theme.ansi[3].to_hex(),
        theme.ansi[4].to_hex(),
        theme.ansi[5].to_hex(),
        theme.ansi[6].to_hex(),
        theme.ansi[7].to_hex(),
        theme.ansi[8].to_hex(),
        theme.ansi[9].to_hex(),
        theme.ansi[10].to_hex(),
        theme.ansi[11].to_hex(),
        theme.ansi[12].to_hex(),
        theme.ansi[13].to_hex(),
        theme.ansi[14].to_hex(),
        theme.ansi[15].to_hex(),
    )
}
