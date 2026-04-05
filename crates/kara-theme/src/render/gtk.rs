use crate::ResolvedTheme;

pub fn render_gtk_settings(theme: &ResolvedTheme) -> String {
    format!(
        "[Settings]
gtk-theme-name={}
gtk-icon-theme-name={}
gtk-cursor-theme-name={}
gtk-cursor-theme-size={}
gtk-application-prefer-dark-theme={}
",
        theme.gtk_theme_name(),
        theme.gtk_icon_theme_name(),
        theme.cursor.theme,
        theme.cursor.size,
        theme.prefer_dark_flag()
    )
}
