use crate::{NvimPreset, ResolvedTheme};

pub fn render_nvim_theme(theme: &ResolvedTheme) -> String {
    let labels = [
        "base00", "base01", "base02", "base03", "base04", "base05", "base06", "base07", "base08",
        "base09", "base0A", "base0B", "base0C", "base0D", "base0E", "base0F",
    ];

    let mut palette = String::new();
    for (idx, name) in labels.iter().enumerate() {
        palette.push_str(&format!(
            "    {} = \"{}\",\n",
            name,
            theme.base16[idx].to_hex()
        ));
    }

    // Dispatch priority:
    //   1. `variant_preset` — when the theme resolved via a hand-tuned
    //      built-in preset (gruvbox/vague/nord/…), use its plugin so
    //      the editor gets a real tuned colorscheme (treesitter groups,
    //      LSP, telescope, etc.) instead of a generic base16 derivation.
    //   2. `nvim_preset` from theme.toml — the legacy `[nvim] preset = ...`
    //      override users can set per-theme.
    //   3. Fall through to `kara-custom`, which kara_theme.lua dispatches
    //      to mini.base16 with the resolved palette.
    //
    // Known plugin colorscheme names here must stay in sync with
    // kara_theme.lua's apply_* dispatch table.
    let known_plugin = match theme.variant_preset.as_deref() {
        Some("gruvbox") => Some("gruvbox"),
        Some("vague") => Some("vague"),
        Some("nord") => Some("nord"),
        _ => None,
    };

    let (colorscheme, variant) = if let Some(name) = known_plugin {
        (
            name,
            match theme.mode {
                crate::UiMode::Light => "light",
                crate::UiMode::Dark | crate::UiMode::Auto => "dark",
            },
        )
    } else {
        match theme.nvim_preset {
            NvimPreset::Gruvbox => ("gruvbox", "dark"),
            NvimPreset::Vague => ("vague", "dark"),
            NvimPreset::Semantic => (
                "kara-custom",
                match theme.mode {
                    crate::UiMode::Light => "light",
                    crate::UiMode::Dark => "dark",
                    crate::UiMode::Auto => "dark",
                },
            ),
        }
    };

    format!(
        "return {{
  engine = \"kara-beautify\",
  name = \"{}\",
  colorscheme = \"{}\",
  variant = \"{}\",
  transparent = {},
  primary = \"{}\",
  palette = {{
{}
  }},
}}
",
        theme.name,
        colorscheme,
        variant,
        if theme.nvim_transparent {
            "true"
        } else {
            "false"
        },
        theme.primary.to_hex(),
        palette
    )
}
