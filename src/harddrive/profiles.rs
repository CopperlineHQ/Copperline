// SPDX-License-Identifier: GPL-3.0-or-later

//! Built-in drive profiles for [`super::mechanics`]: the drives Commodore
//! fitted (as far as owner reports and reviews can tell) and the common
//! period upgrades, from their manufacturers' manuals. `HDD-TIMING-PLAN.md`
//! carries the full source list, grades and the derivation of every
//! estimated figure; the comment on each entry names its source key there.
//!
//! Where a manual is silent the plan's gap rules apply: a 3.0 ms head
//! switch, a 1.0 ms command overhead, and for zoned drives without a
//! published zone table, zones estimated from the media rate at 0.8 format
//! efficiency (or from the published sectors-per-track range).

use super::mechanics::{DriveProfile, Zone};
use crate::config::MachineModel;

struct Spec {
    id: &'static str,
    label: &'static str,
    rpm: u32,
    heads: u32,
    /// (cylinders, sectors per track), outermost first.
    zones: &'static [(u32, u32)],
    seek_us: [u32; 3],
    write_seek_us: Option<[u32; 3]>,
    head_switch_us: u32,
    skew_sectors: Option<(u32, u32)>,
    command_overhead_us: u32,
    buffer_kib: u32,
    prefetch_sectors: Option<u32>,
    read_ahead: bool,
    partial_hits: bool,
    write_cache: bool,
    spin_up_ms: u32,
    standby_spin_up_ms: u32,
    idle_park_ms: Option<u32>,
    scsi_kb_per_s: u32,
}

/// The plan's default head switch, for manuals that do not quote one.
const GAP_HEAD_SWITCH_US: u32 = 3000;
/// The plan's default command overhead (Conner's figure).
const GAP_OVERHEAD_US: u32 = 1000;

const ATA: Spec = Spec {
    id: "",
    label: "",
    rpm: 3600,
    heads: 1,
    zones: &[],
    seek_us: [0, 0, 0],
    write_seek_us: None,
    head_switch_us: GAP_HEAD_SWITCH_US,
    skew_sectors: None,
    command_overhead_us: GAP_OVERHEAD_US,
    buffer_kib: 32,
    prefetch_sectors: None,
    read_ahead: true,
    partial_hits: true,
    write_cache: false,
    spin_up_ms: 10_000,
    standby_spin_up_ms: 10_000,
    idle_park_ms: None,
    scsi_kb_per_s: 0,
};

const BUILTIN: &[Spec] = &[
    // ----- 2.5" ATA (A600, A1200) -----------------------------------------
    // [C24] Conner CP2024 manual: 653 x 2 x 32, 3433 rpm, 8/23/40 ms, 8 KB
    // look-ahead buffer, 7 s start.
    Spec {
        id: "conner-cp2024",
        label: "Conner CP2024, 21 MB 2.5\" ATA (1990)",
        rpm: 3433,
        heads: 2,
        zones: &[(653, 32)],
        seek_us: [8000, 23_000, 40_000],
        buffer_kib: 8,
        spin_up_ms: 7000,
        standby_spin_up_ms: 7000,
        ..ATA
    },
    // [C44] Conner CP2044PK manual: 548 x 4 x 38, 3486 rpm, 5/19/40 ms,
    // 1.0 ms overhead, 32 KB with look-ahead, 10 s start.
    Spec {
        id: "conner-cp2044",
        label: "Conner CP2044, 43 MB 2.5\" ATA (1990)",
        rpm: 3486,
        heads: 4,
        zones: &[(548, 38)],
        seek_us: [5000, 19_000, 40_000],
        command_overhead_us: 1000,
        ..ATA
    },
    // [C64] Conner CP2034/CP2064 manual: 823 x 4 x 38, otherwise as CP2044.
    Spec {
        id: "conner-cp2064",
        label: "Conner CP2064, 64 MB 2.5\" ATA (1990)",
        rpm: 3486,
        heads: 4,
        zones: &[(823, 38)],
        seek_us: [5000, 19_000, 40_000],
        command_overhead_us: 1000,
        ..ATA
    },
    // [C84] Conner CP2084 manual: 1096 x 4 x 38, otherwise as CP2064.
    Spec {
        id: "conner-cp2084",
        label: "Conner CP2084, 85 MB 2.5\" ATA (1991)",
        rpm: 3486,
        heads: 4,
        zones: &[(1096, 38)],
        seek_us: [5000, 19_000, 40_000],
        command_overhead_us: 1000,
        ..ATA
    },
    // [S235] Seagate ST9080A/ST9145A/ST9235A manual: 3449 rpm, 5/16/27 ms
    // typical, 32 KB (64 KB on the larger two) look-ahead buffer, no write
    // cache, 4 s power-on / 2 s standby to ready, heads park after 5 s idle.
    // Zoned with no published table: 16 Mbit/s at 0.8 efficiency gives 54
    // sectors outside, 41 (three quarters) inside, three zones, cylinders
    // fitted to the capacity.
    Spec {
        id: "seagate-st9080a",
        label: "Seagate ST9080A, 64 MB 2.5\" ATA (1992)",
        rpm: 3449,
        heads: 2,
        zones: &[(438, 54), (438, 48), (438, 41)],
        seek_us: [5000, 16_000, 27_000],
        spin_up_ms: 4000,
        standby_spin_up_ms: 2000,
        idle_park_ms: Some(5000),
        ..ATA
    },
    Spec {
        id: "seagate-st9145a",
        label: "Seagate ST9145A, 128 MB 2.5\" ATA (1992)",
        rpm: 3449,
        heads: 4,
        zones: &[(437, 54), (437, 48), (437, 41)],
        seek_us: [5000, 16_000, 27_000],
        buffer_kib: 64,
        spin_up_ms: 4000,
        standby_spin_up_ms: 2000,
        idle_park_ms: Some(5000),
        ..ATA
    },
    Spec {
        id: "seagate-st9235a",
        label: "Seagate ST9235A, 210 MB 2.5\" ATA (1992)",
        rpm: 3449,
        heads: 6,
        zones: &[(478, 54), (478, 48), (478, 41)],
        seek_us: [5000, 16_000, 27_000],
        buffer_kib: 64,
        spin_up_ms: 4000,
        standby_spin_up_ms: 2000,
        idle_park_ms: Some(5000),
        ..ATA
    },
    // [S655] Seagate ST9655AG manual: 3980 rpm, read 6/16/26 and write
    // 7/20/28 ms, 120 KB with look-ahead and write cache on by default, 7 s
    // power-on / 3 s standby to ready, 5 s idle park. Zones estimated from
    // 28.26 Mbit/s as for the ST9080A.
    Spec {
        id: "seagate-st9655ag",
        label: "Seagate ST9655AG, 524 MB 2.5\" ATA (1994)",
        rpm: 3980,
        heads: 8,
        zones: &[(588, 83), (588, 73), (588, 62)],
        seek_us: [6000, 16_000, 26_000],
        write_seek_us: Some([7000, 20_000, 28_000]),
        buffer_kib: 120,
        write_cache: true,
        spin_up_ms: 7000,
        standby_spin_up_ms: 3000,
        idle_park_ms: Some(5000),
        ..ATA
    },
    // [IBM] IBM DPRS (SCSI) spec, the ATA DPRA reported to share the
    // mechanism: 4900 rpm, read 4/12/23 and write 4/13/24 ms, 1.0 ms
    // overhead, 64 KB, 3.8 s start, 2.3 s from standby, track skew 11 and
    // cylinder skew 15. Eight zones from 36.3-57.0 Mbit/s at 0.8 efficiency.
    Spec {
        id: "ibm-dpra-21215",
        label: "IBM DPRA-21215, 1.2 GB 2.5\" ATA (1995)",
        rpm: 4900,
        heads: 6,
        zones: &[
            (447, 136),
            (447, 129),
            (447, 122),
            (447, 115),
            (447, 107),
            (447, 100),
            (447, 93),
            (447, 86),
        ],
        seek_us: [4000, 12_000, 23_000],
        write_seek_us: Some([4000, 13_000, 24_000]),
        skew_sectors: Some((11, 15)),
        command_overhead_us: 1000,
        buffer_kib: 64,
        spin_up_ms: 3800,
        standby_spin_up_ms: 2300,
        ..ATA
    },
    // ----- 3.5" ATA (A4000, lide) -----------------------------------------
    // [REF43] The Ref 4.30 (maintainer-confirmed): 3211 rpm, 3 heads, MZR,
    // 16 ms average, 3.0 ms track-to-track, 12 Mbit/s, 32 KB. Two zones (as
    // the sibling ST3120A): 43 sectors outside from 12 Mbit/s at 0.8
    // efficiency, 32 inside, cylinders fitted to 1001/15/17. Full stroke
    // estimated at 1.8 x average. Serves only complete buffer hits: the
    // period A4000 ScsiSpeed figures lose most of a revolution on every
    // multi-sector request but none on single sectors.
    Spec {
        id: "seagate-st3144a",
        label: "Seagate ST3144A, 130 MB 3.5\" ATA (1991)",
        rpm: 3211,
        heads: 3,
        zones: &[(1135, 43), (1135, 32)],
        seek_us: [3000, 16_000, 29_000],
        partial_hits: false,
        ..ATA
    },
    // [CON] Conner CP30084/CP30104 manual: 1053 / 1524 x 4 x 39 user
    // sectors, 3400 rpm, 8/19/35 ms, 1.0 ms overhead, 64 KB look-ahead,
    // 15 s start.
    Spec {
        id: "conner-cp30084",
        label: "Conner CP30084, 84 MB 3.5\" ATA (1991)",
        rpm: 3400,
        heads: 4,
        zones: &[(1053, 39)],
        seek_us: [8000, 19_000, 35_000],
        command_overhead_us: 1000,
        buffer_kib: 64,
        spin_up_ms: 15_000,
        standby_spin_up_ms: 15_000,
        ..ATA
    },
    Spec {
        id: "conner-cp30104",
        label: "Conner CP30104, 120 MB 3.5\" ATA (1991)",
        rpm: 3400,
        heads: 4,
        zones: &[(1524, 39)],
        seek_us: [8000, 19_000, 35_000],
        command_overhead_us: 1000,
        buffer_kib: 64,
        spin_up_ms: 15_000,
        standby_spin_up_ms: 15_000,
        ..ATA
    },
    // [WD] WD Caviar AC2120 technical reference: 4 heads, zone 1 970 x 48,
    // zone 2 379 x 40, 3605 rpm, 6/15/28 ms, 0.3 ms overhead, 32 KB in four
    // segments with read-ahead, writes not cached, 5 s spin-up.
    Spec {
        id: "wd-ac2120",
        label: "Western Digital Caviar AC2120, 120 MB 3.5\" ATA (1992)",
        rpm: 3605,
        heads: 4,
        zones: &[(970, 48), (379, 40)],
        seek_us: [6000, 15_000, 28_000],
        command_overhead_us: 300,
        prefetch_sectors: Some(16),
        spin_up_ms: 5000,
        standby_spin_up_ms: 5000,
        ..ATA
    },
    // [QFB][DT95Q] Quantum Fireball 1080: 5400 rpm, 3/12/22 ms, 4 heads,
    // 88-177 sectors per track, 128 KB. Zone count unpublished: eight zones
    // spaced evenly across that range, cylinders fitted to the capacity.
    Spec {
        id: "quantum-fireball-1080at",
        label: "Quantum Fireball 1080AT, 1.08 GB 3.5\" ATA (1995)",
        rpm: 5400,
        heads: 4,
        zones: &[
            (503, 177),
            (503, 164),
            (503, 152),
            (503, 139),
            (503, 126),
            (503, 113),
            (503, 101),
            (503, 88),
        ],
        seek_us: [3000, 12_000, 22_000],
        buffer_kib: 128,
        ..ATA
    },
    // ----- 3.5" SCSI (A3000, A2091/A590, A4091) ---------------------------
    // [S157] Seagate ST125N/ST138N/ST157N manual: 407 / 615 cylinders x 4 / 6
    // heads x 26 sectors, 3600 rpm, band-stepper 8/38/90 ms (MLC-0),
    // controller overhead under 3 ms, 1.5 MB/s, ready within 25 s. No buffer
    // or look-ahead documented.
    Spec {
        id: "seagate-st125n",
        label: "Seagate ST125N, 21 MB 3.5\" SCSI (1987)",
        rpm: 3600,
        heads: 4,
        zones: &[(407, 26)],
        seek_us: [8000, 38_000, 90_000],
        command_overhead_us: 2000,
        buffer_kib: 1,
        read_ahead: false,
        spin_up_ms: 25_000,
        standby_spin_up_ms: 25_000,
        scsi_kb_per_s: 1500,
        ..ATA
    },
    Spec {
        id: "seagate-st157n",
        label: "Seagate ST157N, 49 MB 3.5\" SCSI (1987)",
        rpm: 3600,
        heads: 6,
        zones: &[(615, 26)],
        seek_us: [8000, 38_000, 90_000],
        command_overhead_us: 2000,
        buffer_kib: 1,
        read_ahead: false,
        spin_up_ms: 25_000,
        standby_spin_up_ms: 25_000,
        scsi_kb_per_s: 1500,
        ..ATA
    },
    // [Q88] Quantum ProDrive 40S/80S manual: 834 cylinders x 3 / 6 heads in
    // two zones of 35 and 28 sectors (split derived from the 82,029-sector
    // capacity), 3662 rpm, 6/19/35 ms, 3.0 ms head switch, skews 7 and 15,
    // 64 KB DisCache (4 x 16 KB segments, prefetch up to 16 blocks,
    // write-through), 2.0 MB/s asynchronous, 13 s power-up.
    Spec {
        id: "quantum-prodrive-40s",
        label: "Quantum ProDrive 40S, 42 MB 3.5\" SCSI (1988)",
        rpm: 3662,
        heads: 3,
        zones: &[(571, 35), (263, 28)],
        seek_us: [6000, 19_000, 35_000],
        head_switch_us: 3000,
        skew_sectors: Some((7, 15)),
        buffer_kib: 64,
        prefetch_sectors: Some(16),
        spin_up_ms: 13_000,
        standby_spin_up_ms: 13_000,
        scsi_kb_per_s: 2000,
        ..ATA
    },
    Spec {
        id: "quantum-prodrive-80s",
        label: "Quantum ProDrive 80S, 84 MB 3.5\" SCSI (1988)",
        rpm: 3662,
        heads: 6,
        zones: &[(571, 35), (263, 28)],
        seek_us: [6000, 19_000, 35_000],
        head_switch_us: 3000,
        skew_sectors: Some((7, 15)),
        buffer_kib: 64,
        prefetch_sectors: Some(16),
        spin_up_ms: 13_000,
        standby_spin_up_ms: 13_000,
        scsi_kb_per_s: 2000,
        ..ATA
    },
    // [REF43][DT90Q] Quantum ProDrive LPS 52S / 105S: 1219 cylinders x 2 / 4
    // heads, three zones, 3660 rpm, 17 ms average, 5.0 ms track-to-track,
    // 16 Mbit/s, 64 KB. Zones: 51 sectors outside from 16 Mbit/s at 0.8
    // efficiency, stepping evenly to the 42-sector average the capacity
    // implies. Full stroke 1.8 x average; cache policy, async rate and
    // spin-up taken from the ProDrive 40S/80S manual.
    Spec {
        id: "quantum-lps-52s",
        label: "Quantum ProDrive LPS 52S, 52 MB 3.5\" SCSI (1990)",
        rpm: 3660,
        heads: 2,
        zones: &[(407, 51), (406, 42), (406, 33)],
        seek_us: [5000, 17_000, 31_000],
        buffer_kib: 64,
        prefetch_sectors: Some(16),
        spin_up_ms: 13_000,
        standby_spin_up_ms: 13_000,
        scsi_kb_per_s: 2000,
        ..ATA
    },
    Spec {
        id: "quantum-lps-105s",
        label: "Quantum ProDrive LPS 105S, 105 MB 3.5\" SCSI (1990)",
        rpm: 3660,
        heads: 4,
        zones: &[(407, 51), (406, 42), (406, 33)],
        seek_us: [5000, 17_000, 31_000],
        buffer_kib: 64,
        prefetch_sectors: Some(16),
        spin_up_ms: 13_000,
        standby_spin_up_ms: 13_000,
        scsi_kb_per_s: 2000,
        ..ATA
    },
    // [HAWK] Seagate Hawk 1LP manual: 2700 cylinders x 9 heads, 85 sectors
    // per track on average, 26.8-47.2 Mbit/s, 5411 rpm, read 1.2/9.3/19.4
    // and write 1.7/10.5/20.4 ms, 1 ms head switch, about 0.9 ms overhead,
    // 256 KB (240 usable) in 3 segments with prefetch, write cache off,
    // 3.1 MB/s asynchronous average, 15 s ready. Eight zones keeping the
    // published rate ratio, scaled to the 85-sector average.
    Spec {
        id: "seagate-st31200n",
        label: "Seagate Hawk 1LP ST31200N, 1.05 GB 3.5\" SCSI (1995)",
        rpm: 5411,
        heads: 9,
        zones: &[
            (338, 108),
            (338, 101),
            (338, 95),
            (338, 88),
            (338, 82),
            (338, 75),
            (338, 69),
            (338, 62),
        ],
        seek_us: [1200, 9300, 19_400],
        write_seek_us: Some([1700, 10_500, 20_400]),
        head_switch_us: 1000,
        command_overhead_us: 900,
        buffer_kib: 240,
        prefetch_sectors: Some(160),
        spin_up_ms: 15_000,
        standby_spin_up_ms: 15_000,
        scsi_kb_per_s: 3100,
        ..ATA
    },
    // [QFB][DT95Q] The Fireball 1080S is the 1080AT mechanism on SCSI-2;
    // its asynchronous rate is assumed equal to the Hawk's.
    Spec {
        id: "quantum-fireball-1080s",
        label: "Quantum Fireball 1080S, 1.08 GB 3.5\" SCSI (1995)",
        rpm: 5400,
        heads: 4,
        zones: &[
            (503, 177),
            (503, 164),
            (503, 152),
            (503, 139),
            (503, 126),
            (503, 113),
            (503, 101),
            (503, 88),
        ],
        seek_us: [3000, 12_000, 22_000],
        buffer_kib: 128,
        scsi_kb_per_s: 3100,
        ..ATA
    },
];

fn to_profile(spec: &Spec) -> DriveProfile {
    DriveProfile {
        id: spec.id.to_string(),
        label: spec.label.to_string(),
        rpm: spec.rpm,
        heads: spec.heads,
        zones: spec
            .zones
            .iter()
            .map(|&(cylinders, sectors_per_track)| Zone {
                cylinders,
                sectors_per_track,
            })
            .collect(),
        seek_us: spec.seek_us,
        write_seek_us: spec.write_seek_us,
        head_switch_us: spec.head_switch_us,
        skew_sectors: spec.skew_sectors,
        command_overhead_us: spec.command_overhead_us,
        buffer_kib: spec.buffer_kib,
        prefetch_sectors: spec.prefetch_sectors,
        read_ahead: spec.read_ahead,
        partial_hits: spec.partial_hits,
        write_cache: spec.write_cache,
        spin_up_ms: spec.spin_up_ms,
        standby_spin_up_ms: spec.standby_spin_up_ms,
        idle_park_ms: spec.idle_park_ms,
        scsi_kb_per_s: spec.scsi_kb_per_s,
    }
}

/// Every built-in profile, in table order.
pub fn builtin_profiles() -> Vec<DriveProfile> {
    BUILTIN.iter().map(to_profile).collect()
}

/// The built-in profile called `id`.
pub fn builtin_profile(id: &str) -> Option<DriveProfile> {
    BUILTIN.iter().find(|spec| spec.id == id).map(to_profile)
}

/// Which drive interface a drive hangs off, for resolving `timing =
/// "period"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriveBus {
    /// A600/A1200 Gayle IDE.
    Gayle,
    /// A4000 motherboard IDE.
    A4000Ide,
    /// A lide.device-compatible Zorro II IDE board.
    Lide,
    /// A3000 motherboard SCSI (Super DMAC).
    A3000Scsi,
    /// A2091 (or, on an A500-class machine, the A590 side-car).
    A2091,
    /// A4091 Zorro III SCSI.
    A4091,
}

/// The drive `timing = "period"` stands for on `bus` in `machine` (the
/// per-machine defaults table in `HDD-TIMING-PLAN.md`).
pub fn period_profile_id(bus: DriveBus, machine: Option<MachineModel>) -> &'static str {
    match bus {
        // The A600HD launched with 20 MB; the A1200HD's Conner "60 meg"
        // is the CP2064.
        DriveBus::Gayle if machine == Some(MachineModel::A600) => "conner-cp2024",
        DriveBus::Gayle => "conner-cp2064",
        DriveBus::A4000Ide => "seagate-st3144a",
        DriveBus::Lide => "wd-ac2120",
        DriveBus::A3000Scsi => "quantum-lps-52s",
        // An A2091 controller on an A500-class machine is an A590, which
        // shipped a 20 MB Seagate SCSI drive; in a big-box machine it is
        // the A2000HD's 52 MB Quantum.
        DriveBus::A2091 => match machine {
            None
            | Some(MachineModel::A500)
            | Some(MachineModel::A500Plus)
            | Some(MachineModel::A1000)
            | Some(MachineModel::Cdtv) => "seagate-st125n",
            _ => "quantum-lps-52s",
        },
        DriveBus::A4091 => "seagate-st31200n",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_builtin_profile_validates_and_ids_are_unique() {
        let profiles = builtin_profiles();
        for profile in &profiles {
            profile
                .validate()
                .unwrap_or_else(|error| panic!("{}: {error}", profile.id));
        }
        let mut ids: Vec<_> = profiles.iter().map(|p| p.id.as_str()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), profiles.len());
    }

    #[test]
    fn period_defaults_name_builtin_profiles() {
        use DriveBus::*;
        for bus in [Gayle, A4000Ide, Lide, A3000Scsi, A2091, A4091] {
            for machine in [
                None,
                Some(MachineModel::A500),
                Some(MachineModel::A600),
                Some(MachineModel::A1200),
                Some(MachineModel::A3000),
                Some(MachineModel::A4000),
            ] {
                let id = period_profile_id(bus, machine);
                assert!(builtin_profile(id).is_some(), "{id}");
            }
        }
        assert_eq!(
            period_profile_id(Gayle, Some(MachineModel::A600)),
            "conner-cp2024"
        );
        assert_eq!(
            period_profile_id(A2091, Some(MachineModel::A500)),
            "seagate-st125n"
        );
        assert_eq!(
            period_profile_id(A2091, Some(MachineModel::A4000)),
            "quantum-lps-52s"
        );
    }

    #[test]
    fn manual_geometry_matches_the_published_capacity() {
        // Non-zoned profiles taken straight from their manuals.
        for (id, sectors) in [("conner-cp2064", 125_096), ("conner-cp30104", 237_744)] {
            assert_eq!(
                builtin_profile(id).unwrap().capacity_sectors(),
                sectors,
                "{id}"
            );
        }
    }
}
