use std::env;
use std::fs;
use std::path::PathBuf;

use anyhow::{Result, anyhow};

/// Where a theme came from in the search path. Printed by `list`
/// and used in error messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeSource {
    User,
    Data,
    Repo,
    System,
}

impl ThemeSource {
    pub fn label(self) -> &'static str {
        match self {
            ThemeSource::User => "user",
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
    /// First match wins — a theme in `~/.config/kara/themes/` overrides
    /// one of the same name bundled with the repo or installed system-wide.
    ///
    /// Users who want to author their own theme drop a directory into
    /// `~/.config/kara/themes/<name>/` with a `theme.toml` inside. No
    /// kara rebuild needed.
    ///
    /// Search order:
    ///   1. `$XDG_CONFIG_HOME/kara/themes/` — user-authored (highest)
    ///   2. `$XDG_DATA_HOME/kara/themes/` — installed via theme fetch
    ///   3. `<repo_root>/themes/` — dev mode when running from source
    ///   4. `$XDG_DATA_DIRS/kara/themes/` for each entry in $XDG_DATA_DIRS
    ///      (typically `/usr/local/share:/usr/share`) — system install
    ///
    /// Iterating XDG_DATA_DIRS means `make install PREFIX=/usr/local`
    /// lands themes at `/usr/local/share/kara/themes/` and a
    /// distribution package that installs to `/usr/share/kara/themes/`
    /// both work without anyone having to hand-edit search paths.
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

        out.push((
            ThemeSource::User,
            self.config_home.join("kara").join("themes"),
        ));
        out.push((
            ThemeSource::Data,
            self.data_home.join("kara").join("themes"),
        ));

        if let Some(root) = repo_root {
            let bundled = root.join("themes");
            if bundled.is_dir() {
                out.push((ThemeSource::Repo, bundled));
            }
        }

        let data_dirs = env::var("XDG_DATA_DIRS")
            .unwrap_or_else(|_| "/usr/local/share:/usr/share".to_string());
        for dir in data_dirs.split(':') {
            if dir.is_empty() {
                continue;
            }
            out.push((
                ThemeSource::System,
                PathBuf::from(dir).join("kara").join("themes"),
            ));
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
        self.data_home.join("bg")
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
