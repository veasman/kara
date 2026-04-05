use anyhow::{Result, bail};

use kara_color::Color;
use crate::ThemeSpec;

pub fn validate_spec(spec: &ThemeSpec) -> Result<()> {
    if spec.meta.name.trim().is_empty() {
        bail!("theme name cannot be empty");
    }

    let _ = Color::from_hex(&spec.palette.primary)?;

    if !(0.0..=1.0).contains(&spec.style.transparency) {
        bail!("style.transparency must be between 0.0 and 1.0");
    }

    if spec.fonts.ui_size == 0 || spec.fonts.mono_size == 0 {
        bail!("font sizes must be > 0");
    }

    Ok(())
}
