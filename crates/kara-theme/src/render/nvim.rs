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

    let (colorscheme, variant) = match theme.nvim_preset {
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
