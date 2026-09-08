// SPDX-License-Identifier: GPL-3.0-or-later

//! Small, host preferences independent of egui storage. No egui memory, command text,
//! captured data, or machine state is serialized here.

use super::*;
use serde::{Deserialize, Serialize};
use std::{io::Write, path::Path};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(in crate::video::window) struct Preferences {
    pub workspace_size: [f64; 2],
    pub display_width: f32,
    pub register_width: f32,
    pub memory_height: f32,
    debugger_tab: String,
    analyzer_tab: String,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            workspace_size: [1440.0, 900.0],
            display_width: 560.0,
            register_width: 235.0,
            memory_height: 190.0,
            debugger_tab: "CPU".into(),
            analyzer_tab: "Beam".into(),
        }
    }
}

impl Preferences {
    fn sanitized(mut self) -> Self {
        fn bounded(value: f64, low: f64, high: f64, default: f64) -> f64 {
            if value.is_finite() {
                value.clamp(low, high)
            } else {
                default
            }
        }
        self.workspace_size[0] = bounded(self.workspace_size[0], 900.0, 4096.0, 1440.0);
        self.workspace_size[1] = bounded(self.workspace_size[1], 600.0, 2160.0, 900.0);
        self.display_width = bounded(self.display_width as f64, 240.0, 2400.0, 560.0) as f32;
        self.register_width = bounded(self.register_width as f64, 180.0, 480.0, 235.0) as f32;
        self.memory_height = bounded(self.memory_height as f64, 100.0, 480.0, 190.0) as f32;
        self
    }

    pub(super) fn load(path: &Path) -> Self {
        // A partially written, malformed, or oversized preferences file must
        // never prevent the debugger from opening.
        std::fs::metadata(path)
            .ok()
            .filter(|meta| meta.len() <= 16_384)
            .and_then(|_| std::fs::read_to_string(path).ok())
            .and_then(|text| toml::from_str::<Self>(&text).ok())
            .unwrap_or_default()
            .sanitized()
    }

    pub(super) fn save(&self, path: &Path) -> anyhow::Result<()> {
        crate::paths::ensure_parent(path)?;
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        let mut file = tempfile::NamedTempFile::new_in(parent)?;
        file.write_all(toml::to_string_pretty(&self.clone().sanitized())?.as_bytes())?;
        file.persist(path)?;
        Ok(())
    }

    pub(in crate::video::window) fn debugger_tab(&self) -> ui::DebugTab {
        ui::DEBUG_TABS
            .into_iter()
            .find(|tab| ui::debug_tab_label(*tab) == self.debugger_tab)
            .unwrap_or(ui::DebugTab::Cpu)
    }

    pub(in crate::video::window) fn analyzer_tab(&self) -> ui::AnalyzerTab {
        ui::ANALYZER_TABS
            .into_iter()
            .find(|tab| ui::analyzer_tab_label(*tab) == self.analyzer_tab)
            .unwrap_or(ui::AnalyzerTab::Beam)
    }
}

fn preference_path() -> Option<std::path::PathBuf> {
    // Unit tests use explicit temporary paths, never the maintainer's layout.
    if cfg!(test) {
        None
    } else {
        crate::paths::config_file("inspector-layout.toml")
    }
}

impl App {
    pub(in crate::video::window) fn egui_layout_preferences(&mut self) -> &Preferences {
        self.egui_preferences.get_or_insert_with(|| {
            preference_path()
                .map(|path| Preferences::load(&path))
                .unwrap_or_default()
        })
    }

    pub(in crate::video::window) fn save_egui_preferences(&mut self) {
        let Some(ui) = &mut self.debugger_ui else {
            return;
        };
        let mut preferences = ui.layout.preferences.clone();
        if self.debug_layout_active {
            if let Some(r) = &self.render {
                if !r.minimized && !r.window.is_maximized() && r.window.fullscreen().is_none() {
                    let size = r
                        .window
                        .inner_size()
                        .to_logical::<f64>(r.window.scale_factor());
                    preferences.workspace_size = [size.width, size.height];
                }
            }
        }
        if let Some(panel) = &self.debugger_panel {
            preferences.debugger_tab = ui::debug_tab_label(panel.tab).to_owned();
        }
        if let Some(panel) = &self.frame_analyzer_panel {
            preferences.analyzer_tab = ui::analyzer_tab_label(panel.tab).to_owned();
        }
        self.egui_preferences = Some(preferences.clone());
        ui.layout.preferences = preferences.clone();
        if let Some(path) = preference_path() {
            if let Err(error) = preferences.save(&path) {
                log::warn!(
                    "could not save inspector layout ({}): {error}",
                    path.display()
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_round_trip_recovers_from_invalid_and_legacy_files() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("layout.toml");
        let prefs = Preferences {
            workspace_size: [1450.0, 820.0],
            display_width: 620.0,
            register_width: 310.0,
            memory_height: 240.0,
            debugger_tab: "Video".into(),
            analyzer_tab: "Memory".into(),
        };
        prefs.save(&path).unwrap();
        assert_eq!(Preferences::load(&path), prefs);
        std::fs::write(&path, "not toml").unwrap();
        assert_eq!(Preferences::load(&path), Preferences::default());
        std::fs::write(
            &path,
            "workspace_size = [nan, -42.0]\ndisplay_width = inf\nregister_width = inf\nanalyzer_tab = 'Unknown'",
        )
        .unwrap();
        let recovered = Preferences::load(&path);
        assert_eq!(recovered.workspace_size, [1440.0, 600.0]);
        assert_eq!(recovered.display_width, 560.0);
        assert_eq!(recovered.register_width, 235.0);
        assert_eq!(recovered.analyzer_tab(), ui::AnalyzerTab::Beam);
        std::fs::write(
            &path,
            "window_size = [1100.0, 760.0]\nposition = [-1400, 60]\nregister_width = 310.0",
        )
        .unwrap();
        let legacy = Preferences::load(&path);
        assert_eq!(legacy.workspace_size, Preferences::default().workspace_size);
        assert_eq!(legacy.register_width, 310.0);
    }
}
