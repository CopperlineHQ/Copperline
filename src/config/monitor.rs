// SPDX-License-Identifier: GPL-3.0-or-later

//! Host display selection, independent of the desktop windowing backend.

use std::{fmt, str::FromStr};

use anyhow::{bail, Result};

/// Where the desktop frontend opens its main window. Numbers are one-based
/// positions in `--list-monitors`; names are matched exactly.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum HostMonitor {
    #[default]
    Auto,
    Primary,
    Index(usize),
    Name(String),
}

impl FromStr for HostMonitor {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        let value = value.trim();
        if let Some(name) = value.strip_prefix("name:") {
            if name.is_empty() {
                bail!("monitor name must not be empty");
            }
            return Ok(Self::Name(name.to_string()));
        }
        if value.eq_ignore_ascii_case("auto") {
            return Ok(Self::Auto);
        }
        if value.eq_ignore_ascii_case("primary") {
            return Ok(Self::Primary);
        }
        if value.is_empty() {
            bail!("monitor must be auto, primary, a number starting at 1, or an exact name");
        }
        let numeric = value.trim_start_matches(['-', '+']);
        if numeric.bytes().all(|b| b.is_ascii_digit()) {
            let index = value.parse::<usize>()?;
            if index == 0 {
                bail!("monitor numbers start at 1; use --list-monitors to list them");
            }
            return Ok(Self::Index(index));
        }
        Ok(Self::Name(value.to_string()))
    }
}

impl fmt::Display for HostMonitor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Auto => f.write_str("auto"),
            Self::Primary => f.write_str("primary"),
            Self::Index(index) => write!(f, "{index}"),
            // The prefix disambiguates names such as "primary" or "2".
            Self::Name(name) => write!(f, "name:{name}"),
        }
    }
}
