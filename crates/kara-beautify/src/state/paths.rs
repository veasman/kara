use std::env;
use std::fs;
use std::path::PathBuf;

use anyhow::{Result, anyhow};

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
