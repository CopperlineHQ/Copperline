// SPDX-License-Identifier: GPL-3.0-or-later

//! Small, host preferences independent of egui storage. No egui memory, command text,
//! captured data, or machine state is serialized here.

use super::*;
use serde::{Deserialize, Serialize};
use std::{io::Write, path::Path};
use winit::dpi::{LogicalSize, PhysicalPosition};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(in crate::video::window) struct Preferences {
    pub window_size: [f64; 2],
    pub position: Option<[i32; 2]>,
    pub maximized: bool,
    pub register_width: f32,
    pub memory_height: f32,
    debugger_tab: String,
    analyzer_tab: String,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            window_size: [1100.0, 760.0],
            position: None,
            maximized: false,
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
        self.window_size[0] = bounded(self.window_size[0], 600.0, 4096.0, 1100.0);
        self.window_size[1] = bounded(self.window_size[1], 480.0, 2160.0, 760.0);
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

    pub(in crate::video::window) fn window_attributes(
        &self,
        mut attrs: winit::window::WindowAttributes,
        event_loop: &winit::event_loop::ActiveEventLoop,
    ) -> winit::window::WindowAttributes {
        attrs = attrs
            .with_inner_size(LogicalSize::new(self.window_size[0], self.window_size[1]))
            .with_maximized(self.maximized);
        if let Some([x, y]) = self.position {
            // Restore only when the title bar remains reachable. On Wayland
            // outer_position is unavailable and position stays None.
            let visible = event_loop.available_monitors().any(|monitor| {
                let pos = monitor.position();
                let size = monitor.size();
                title_bar_visible([x, y], [pos.x, pos.y], [size.width, size.height])
            });
            if visible {
                attrs = attrs.with_position(PhysicalPosition::new(x, y));
            }
        }
        attrs
    }
}

fn title_bar_visible(position: [i32; 2], monitor: [i32; 2], size: [u32; 2]) -> bool {
    let [x, y] = position.map(i64::from);
    let [left, top] = monitor.map(i64::from);
    x >= left
        && x + 160 <= left + i64::from(size[0])
        && y >= top
        && y + 64 <= top + i64::from(size[1])
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
        let Some(tool) = &self.debugger_tool_window else {
            return;
        };
        let egui = &tool.egui;
        let mut preferences = egui.layout.preferences.clone();
        preferences.maximized = tool.window.is_maximized();
        if !tool.minimized && !preferences.maximized {
            let size = tool
                .window
                .inner_size()
                .to_logical::<f64>(tool.window.scale_factor());
            preferences.window_size = [size.width, size.height];
            preferences.position = tool.window.outer_position().ok().map(|p| [p.x, p.y]);
        }
        if let Some(panel) = &self.debugger_panel {
            preferences.debugger_tab = ui::debug_tab_label(panel.tab).to_owned();
        }
        if let Some(panel) = &self.frame_analyzer_panel {
            preferences.analyzer_tab = ui::analyzer_tab_label(panel.tab).to_owned();
        }
        self.egui_preferences = Some(preferences.clone());
        // Keep tab preferences when another logical inspector closes later.
        self.debugger_tool_window
            .as_mut()
            .unwrap()
            .egui
            .layout
            .preferences = preferences.clone();
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
    fn layout_round_trip_recovers_from_invalid_files_and_removed_monitors() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("layout.toml");
        let prefs = Preferences {
            window_size: [1250.0, 820.0],
            register_width: 310.0,
            memory_height: 240.0,
            debugger_tab: "Video".into(),
            analyzer_tab: "Memory".into(),
            position: Some([-1400, 60]),
            ..Default::default()
        };
        prefs.save(&path).unwrap();
        assert_eq!(Preferences::load(&path), prefs);
        std::fs::write(&path, "not toml").unwrap();
        assert_eq!(Preferences::load(&path), Preferences::default());
        std::fs::write(
            &path,
            "window_size = [nan, -42.0]\nregister_width = inf\nanalyzer_tab = 'Unknown'",
        )
        .unwrap();
        let recovered = Preferences::load(&path);
        assert_eq!(recovered.window_size, [1100.0, 480.0]);
        assert_eq!(recovered.register_width, 235.0);
        assert_eq!(recovered.analyzer_tab(), ui::AnalyzerTab::Beam);
        assert!(title_bar_visible([-1400, 60], [-1920, 0], [1920, 1080]));
        assert!(!title_bar_visible([-1400, 60], [0, 0], [1920, 1080]));
        assert!(!title_bar_visible([1900, 1060], [0, 0], [1920, 1080]));
    }
}
