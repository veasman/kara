use std::env;
use std::fs;
use std::path::PathBuf;

use anyhow::{Result, anyhow};

/// Where a theme came from in the search path. Printed by `list`
/// and used in error messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeSource {
    Data,
    Repo,
    System,
}

impl ThemeSource {
    pub fn label(self) -> &'static str {
        match self {
            ThemeSource::Data => "data",
            ThemeSource::Repo => "repo",
            ThemeSource::System => "system",
        }
    }
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct KaraPaths {
    pub home: PathBuf,
    pub config_home: PathBuf,
    pub state_home: PathBuf,
    pub data_home: PathBuf,
}

impl KaraPaths {
    pub fn from_env() -> Result<Self> {
        let home = env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| anyhow!("HOME is not set"))?;

        let config_home = env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"));

        let state_home = env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local").join("state"));

        let data_home = env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local").join("share"));

        Ok(Self {
            home,
            config_home,
            state_home,
            data_home,
        })
    }

    pub fn kara_state_dir(&self) -> PathBuf {
        self.state_home.join("kara")
    }

    /// Directories searched for theme packages, in priority order.
    /// First match wins.
    ///
    /// Kara uses two locations:
    ///
    ///   1. `$XDG_DATA_HOME/kara/themes/` — user-writable (highest).
    ///      Users drop custom themes here, kara-beautify writes
    ///      per-user overrides here, and a user copy shadows any
    ///      same-named system theme.
    ///   2. `$KARA_SYSTEM_THEMES_DIR` or `/usr/share/kara/themes` or
    ///      `/usr/local/share/kara/themes` — bundled with the kara
    ///      install. `make install` ships the repo's themes/ here.
    ///
    /// Dev mode: if the caller passes a `repo_root`, kara also
    /// searches `<repo>/themes/` so `cargo run -p kara-gate` picks
    /// up repo-bundled themes without needing a system install.
    pub fn theme_search_paths(&self, repo_root: Option<&std::path::Path>) -> Vec<PathBuf> {
        self.theme_search_paths_labeled(repo_root)
            .into_iter()
            .map(|(_, p)| p)
            .collect()
    }

    /// Same as `theme_search_paths` but each entry is tagged with its
    /// source label. Used by `list` to print where each theme comes
    /// from and by log/error messages.
    pub fn theme_search_paths_labeled(
        &self,
        repo_root: Option<&std::path::Path>,
    ) -> Vec<(ThemeSource, PathBuf)> {
        let mut out = Vec::new();

        // Primary location — the only place kara actually expects
        // user-visible themes to live.
        out.push((
            ThemeSource::Data,
            self.data_home.join("kara").join("themes"),
        ));

        // Dev fallback — if the caller tells us we're running from a
        // repo checkout, fall back to the bundled themes so dev
        // workflows don't need an install step.
        if let Some(root) = repo_root {
            let bundled = root.join("themes");
            if bundled.is_dir() {
                out.push((ThemeSource::Repo, bundled));
            }
        }

        // System install — picked up from $KARA_SYSTEM_THEMES_DIR
        // (so tests and unusual prefixes can override), otherwise
        // the two common Linux install prefixes. Only the first
        // one that actually exists is added; we don't want the
        // picker to show duplicate entries if both /usr/share and
        // /usr/local/share somehow have content.
        let system_candidates: Vec<PathBuf> = if let Ok(custom) =
            std::env::var("KARA_SYSTEM_THEMES_DIR")
        {
            vec![PathBuf::from(custom)]
        } else {
            vec![
                PathBuf::from("/usr/share/kara/themes"),
                PathBuf::from("/usr/local/share/kara/themes"),
            ]
        };
        for p in system_candidates {
            if p.is_dir() {
                out.push((ThemeSource::System, p));
                break;
            }
        }

        out
    }

    /// Find a theme package by name. Returns the theme directory
    /// (containing theme.toml + wallpapers/) of the first hit in the
    /// search path.
    pub fn find_theme(
        &self,
        name: &str,
        repo_root: Option<&std::path::Path>,
    ) -> Option<PathBuf> {
        for base in self.theme_search_paths(repo_root) {
            let candidate = base.join(name);
            if candidate.join("theme.toml").is_file() {
                return Some(candidate);
            }
        }
        None
    }

    pub fn generated_dir(&self) -> PathBuf {
        self.kara_state_dir().join("generated")
    }

    pub fn themes_state_dir(&self) -> PathBuf {
        self.kara_state_dir().join("themes")
    }

    pub fn current_theme_file(&self) -> PathBuf {
        self.kara_state_dir().join("current_theme")
    }

    pub fn current_wallpaper_file(&self) -> PathBuf {
        self.kara_state_dir().join("current_wallpaper")
    }

    pub fn current_variant_file(&self) -> PathBuf {
        self.kara_state_dir().join("current_variant")
    }

    pub fn preview_active_file(&self) -> PathBuf {
        self.kara_state_dir().join("preview_active")
    }

    pub fn preview_original_theme_file(&self) -> PathBuf {
        self.kara_state_dir().join("preview_original_theme")
    }

    pub fn preview_original_wallpaper_file(&self) -> PathBuf {
        self.kara_state_dir().join("preview_original_wallpaper")
    }

    pub fn current_wallpaper_link(&self) -> PathBuf {
        // ~/.local/share/kara/bg (not ~/.local/share/bg — the
        // bare-path was a leftover from the pre-kara-subdir layout).
        // kara-gate's load_startup_wallpaper() looks here for its
        // initial texture, so this path has to match that lookup.
        self.data_home.join("kara").join("bg")
    }

    pub fn gtk3_settings_path(&self) -> PathBuf {
        self.config_home.join("gtk-3.0").join("settings.ini")
    }

    pub fn gtk4_settings_path(&self) -> PathBuf {
        self.config_home.join("gtk-4.0").join("settings.ini")
    }

    pub fn kara_gate_theme_path(&self) -> PathBuf {
        self.generated_dir().join("kara-gate-theme.conf")
    }

    pub fn kitty_theme_path(&self) -> PathBuf {
        self.generated_dir().join("kitty-theme.conf")
    }

    pub fn foot_theme_path(&self) -> PathBuf {
        self.generated_dir().join("foot-theme.ini")
    }

    /// User's main foot config. kara-beautify patches the
    /// [colors-dark] / [colors-light] section here in place via
    /// ini_patch, because foot's `include=` directive is not
    /// re-read on SIGUSR1 reload. Same pattern as GTK settings.ini.
    pub fn foot_config_path(&self) -> PathBuf {
        self.config_home.join("foot").join("foot.ini")
    }

    /// Root of the Floorp profile tree. Currently hardcoded to
    /// `~/.floorp` — future work may support profile_path override
    /// from kara-beautify.toml for users with a non-default install.
    pub fn floorp_root(&self) -> PathBuf {
        self.home.join(".floorp")
    }

    pub fn nvim_theme_path(&self) -> PathBuf {
        self.generated_dir().join("nvim-theme.lua")
    }

    pub fn tmux_theme_path(&self) -> PathBuf {
        self.generated_dir().join("tmux-theme.conf")
    }

    pub fn fzf_theme_path(&self) -> PathBuf {
        self.generated_dir().join("fzf-theme.sh")
    }

    pub fn session_theme_path(&self) -> PathBuf {
        self.generated_dir().join("session-theme.sh")
    }

    pub fn ensure_runtime_dirs(&self) -> Result<()> {
        fs::create_dir_all(self.generated_dir())?;
        fs::create_dir_all(self.themes_state_dir())?;
        fs::create_dir_all(
            self.current_wallpaper_link()
                .parent()
                .unwrap_or(&self.data_home),
        )?;
        fs::create_dir_all(self.gtk3_settings_path().parent().unwrap())?;
        fs::create_dir_all(self.gtk4_settings_path().parent().unwrap())?;
        Ok(())
    }
}
