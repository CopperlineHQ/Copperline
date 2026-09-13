// SPDX-License-Identifier: GPL-3.0-or-later

//! Replace a software adapter without overlapping surfaces on one window.

use pixels::wgpu::{AdapterInfo, Backend, DeviceType};

/// wgpu can prefer a primary backend's CPU adapter over an accelerated GL
/// adapter. Try GL before building any renderer-dependent UI resources. Drop
/// each renderer before building another surface for the same native window;
/// if GL fails or is also software, rebuild using the original backend.
/// Explicit backend/adapter selections and non-Windows hosts opt out.
pub(super) fn prefer_hardware_renderer<T, E: std::fmt::Display>(
    renderer: T,
    automatic_fallback: bool,
    adapter_info: impl Fn(&T) -> AdapterInfo,
    mut build: impl FnMut(Backend) -> Result<T, E>,
) -> Result<T, E> {
    let initial = adapter_info(&renderer);
    if !automatic_fallback
        || initial.device_type != DeviceType::Cpu
        || initial.backend == Backend::Gl
    {
        return Ok(renderer);
    }

    log::info!(
        "window adapter {:?} ({:?}) uses software rendering; trying OpenGL",
        initial.name,
        initial.backend,
    );
    // Multiple live surfaces for one native window are not supported. The
    // adapter metadata above owns no renderer or surface resources.
    drop(renderer);
    match build(Backend::Gl) {
        Ok(candidate) => {
            let alternative = adapter_info(&candidate);
            if alternative.device_type != DeviceType::Cpu {
                log::info!(
                    "window adapter: using OpenGL adapter {:?} ({:?}) instead of software adapter {:?}",
                    alternative.name,
                    alternative.device_type,
                    initial.name,
                );
                return Ok(candidate);
            }
            log::info!(
                "OpenGL adapter {:?} also uses software rendering; restoring {:?}",
                alternative.name,
                initial.name,
            );
            drop(candidate);
        }
        Err(error) => log::warn!(
            "OpenGL renderer unavailable: {error}; restoring software adapter {:?}",
            initial.name,
        ),
    }
    build(initial.backend)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn adapter(backend: Backend, device_type: DeviceType) -> AdapterInfo {
        AdapterInfo {
            name: String::new(),
            vendor: 0,
            device: 0,
            backend,
            device_type,
            device_pci_bus_id: String::new(),
            driver: String::new(),
            driver_info: String::new(),
            subgroup_min_size: 0,
            subgroup_max_size: 0,
            transient_saves_memory: false,
        }
    }

    #[test]
    fn software_adapter_can_be_replaced_by_an_unclassified_gl_driver() {
        // GL classifies unfamiliar hardware (including virtual drivers) as
        // Other. Requiring DiscreteGpu/IntegratedGpu would miss those drivers.
        let original = adapter(Backend::Dx12, DeviceType::Cpu);
        let replacement = adapter(Backend::Gl, DeviceType::Other);
        let selected = prefer_hardware_renderer(original, true, Clone::clone, |backend| {
            assert_eq!(backend, Backend::Gl);
            Ok::<_, &str>(replacement.clone())
        })
        .unwrap();
        assert_eq!(selected.backend, Backend::Gl);
        assert_eq!(selected.device_type, DeviceType::Other);
    }

    #[test]
    fn failed_or_software_gl_candidate_rebuilds_the_original_backend() {
        for candidate in [
            Err("no compatible GL surface"),
            Err("GL device creation failed"),
            Ok(adapter(Backend::Gl, DeviceType::Cpu)),
        ] {
            let original = adapter(Backend::Dx12, DeviceType::Cpu);
            let mut attempts = Vec::new();
            let selected =
                prefer_hardware_renderer(original.clone(), true, Clone::clone, |backend| {
                    attempts.push(backend);
                    if backend == Backend::Gl {
                        candidate.clone()
                    } else {
                        assert_eq!(backend, original.backend);
                        Ok(original.clone())
                    }
                })
                .unwrap();
            assert_eq!(selected, original);
            assert_eq!(attempts, [Backend::Gl, Backend::Dx12]);
        }
    }

    #[test]
    fn explicit_selection_and_existing_gpu_do_not_initialize_another_backend() {
        let attempts = Cell::new(0);
        for (automatic, backend, device) in [
            (false, Backend::Dx12, DeviceType::Cpu),
            (true, Backend::Metal, DeviceType::IntegratedGpu),
            (true, Backend::Vulkan, DeviceType::DiscreteGpu),
            (true, Backend::Dx12, DeviceType::VirtualGpu),
            (true, Backend::Gl, DeviceType::Cpu),
        ] {
            let original = adapter(backend, device);
            let selected =
                prefer_hardware_renderer(original.clone(), automatic, Clone::clone, |_| {
                    attempts.set(attempts.get() + 1);
                    Err::<AdapterInfo, _>("unexpected backend initialization")
                })
                .unwrap();
            assert_eq!(selected, original);
        }
        assert_eq!(attempts.get(), 0);
    }

    #[test]
    fn surfaces_never_overlap_during_replacement_or_restoration() {
        struct Renderer<'a> {
            info: AdapterInfo,
            live: &'a Cell<usize>,
        }
        impl Drop for Renderer<'_> {
            fn drop(&mut self) {
                self.live.set(self.live.get() - 1);
            }
        }
        let live = Cell::new(0);
        let create = |backend, device_type| {
            assert_eq!(live.get(), 0, "previous surface must be dropped first");
            live.set(1);
            Renderer {
                info: adapter(backend, device_type),
                live: &live,
            }
        };
        for gl_device in [Some(DeviceType::Other), Some(DeviceType::Cpu), None] {
            let original = create(Backend::Dx12, DeviceType::Cpu);
            let selected = prefer_hardware_renderer(
                original,
                true,
                |renderer| renderer.info.clone(),
                |backend| {
                    assert_eq!(live.get(), 0);
                    if backend == Backend::Gl {
                        gl_device
                            .map(|device| create(backend, device))
                            .ok_or("GL initialization failed")
                    } else {
                        Ok(create(backend, DeviceType::Cpu))
                    }
                },
            )
            .unwrap();
            assert_eq!(live.get(), 1);
            drop(selected);
            assert_eq!(live.get(), 0);
        }
    }

    #[test]
    fn restoration_failure_is_returned_to_the_window_creator() {
        let original = adapter(Backend::Dx12, DeviceType::Cpu);
        let result = prefer_hardware_renderer(original, true, Clone::clone, |backend| {
            Err::<AdapterInfo, _>(match backend {
                Backend::Gl => "GL initialization failed",
                Backend::Dx12 => "original backend could not be restored",
                _ => unreachable!(),
            })
        });
        assert_eq!(
            result.unwrap_err(),
            "original backend could not be restored"
        );
    }
}
