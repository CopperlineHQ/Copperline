// SPDX-License-Identifier: GPL-3.0-or-later

//! Tests for the floppy drive/controller and image-format decoders.

use super::formats::*;
use super::*;
use flate2::{write::GzEncoder, Compression};
use std::fs;
use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A write reaches a real platter a revolution after it is handed over,
/// and until it does the drive still holds the recording from *before*
/// it. Reading in that window would show the guest a disk that no longer
/// exists -- and worse, a recording that proved out would be filed as
/// faithful and served from memory indefinitely. So a track is held
/// unreadable from the moment its write is accepted until the drive says
/// what became of it, whether that is success or failure.
#[cfg(feature = "fluxbridge")]
#[test]
fn a_track_being_written_is_not_read_from_the_drive() {
    let mut drive = FloppyDrive::default();
    let track = 7;

    // Nothing pending: an ordinary read may proceed.
    assert!(!drive.bridge_writing.contains(&track));

    // The write is accepted, so the track is held.
    drive.bridge_writing.push(track);
    assert!(
        drive.bridge_writing.contains(&track),
        "an accepted write must hold its track"
    );
    // ...and only that track.
    assert!(!drive.bridge_writing.contains(&(track + 1)));

    // The outcome releases it, and a failed write releases it too: either
    // way the platter now holds something the guest has not seen.
    for outcome_is_failure in [false, true] {
        drive.bridge_writing.clear();
        drive.bridge_writing.push(track);
        drive.bridge_tracks.resize(track + 1, BridgeTrack::Unknown);
        drive.bridge_tracks[track] = BridgeTrack::Kept(CachedTrack::default());
        drive.cached_track = Some(track);

        drive.bridge_writing.retain(|pending| *pending != track);
        drive.bridge_tracks[track] = BridgeTrack::Unknown;
        drive.cached_track = None;

        assert!(
            !drive.bridge_writing.contains(&track),
            "{outcome_is_failure}"
        );
        assert!(
            matches!(drive.bridge_tracks[track], BridgeTrack::Unknown),
            "the recording from before the write must not survive it"
        );
        assert_eq!(drive.cached_track, None);
    }
}

/// A part-captured revolution stands in for its track between polls. It
/// must survive the ticks in between: without a marker saying which track
/// it belongs to, the next tick clears the cache and the head has nothing
/// to read until the following poll, which is most of the window.
#[cfg(feature = "fluxbridge")]
#[test]
fn a_partial_capture_is_retained_between_ticks() {
    let mut drive = FloppyDrive::default();
    let track = 12;
    drive.bridge_partial_track = Some(track);
    drive.bridge_filler_track = None;

    // The retention test the tick applies, for this track and another.
    let keep_this =
        drive.bridge_filler_track == Some(track) || drive.bridge_partial_track == Some(track);
    assert!(keep_this, "a partial must be kept for the track it serves");
    let other = track + 1;
    let keep_other =
        drive.bridge_filler_track == Some(other) || drive.bridge_partial_track == Some(other);
    assert!(!keep_other, "and only for that track");

    // The finished revolution supersedes it.
    drive.bridge_partial_track = None;
    assert!(
        !(drive.bridge_filler_track == Some(track) || drive.bridge_partial_track == Some(track))
    );
}

fn tick_index_flag_sync(ctrl: &mut FloppyController) {
    ctrl.tick(INDEX_FLAG_SYNC_CCK, 0, &mut []);
}

fn clear_index_flag(ctrl: &mut FloppyController) {
    if ctrl.index_flag_sync_cck != 0 {
        tick_index_flag_sync(ctrl);
    }
    ctrl.take_index_pulse();
}

fn bytes_to_words(bytes: &[u8]) -> Vec<u16> {
    bytes
        .chunks_exact(2)
        .map(|chunk| u16::from_be_bytes([chunk[0], chunk[1]]))
        .collect()
}

fn drive_select_prb(idx: usize, motor_on: bool) -> u8 {
    let mut prb = !CIAB_DSKSEL_MASKS[idx];
    if motor_on {
        prb &= !CIAB_DSKMOTOR;
    }
    prb
}

fn drive_deselect_prb(motor_on: bool) -> u8 {
    if motor_on {
        !CIAB_DSKMOTOR
    } else {
        0xFF
    }
}

fn read_external_drive_id(ctrl: &mut FloppyController, idx: usize) -> u32 {
    ctrl.write_prb(drive_deselect_prb(true));
    ctrl.write_prb(drive_select_prb(idx, true));
    ctrl.write_prb(drive_deselect_prb(true));
    ctrl.write_prb(drive_select_prb(idx, false));
    ctrl.write_prb(drive_deselect_prb(false));

    let mut id = 0u32;
    for _ in 0..32 {
        ctrl.write_prb(drive_select_prb(idx, false));
        id = (id << 1) | u32::from(ctrl.cia_a_status_bits() & CIAA_DSKRDY == 0);
        ctrl.write_prb(drive_deselect_prb(false));
    }
    id
}

#[test]
fn standard_adf_geometry_matches_expected_size() {
    assert_eq!(ADF_SIZE, 901_120);
    assert_eq!(adf_sector_offset(159, 10) + BYTES_PER_SECTOR, ADF_SIZE);
}

#[test]
fn mfm_encode_decode_round_trip_sector_data() -> Result<()> {
    let mut adf = vec![0u8; ADF_SIZE];
    for (i, b) in adf.iter_mut().take(BYTES_PER_SECTOR).enumerate() {
        *b = i as u8;
    }
    let words = encode_adf_track(0, &adf);
    let decoded = decode_track_write(0, &words)?;
    let sector0 = decoded.iter().find(|(sector, _)| *sector == 0).unwrap();
    assert_eq!(&sector0.1[..], &adf[0..BYTES_PER_SECTOR]);
    assert_eq!(decoded.len(), SECTORS_PER_TRACK);
    Ok(())
}

#[test]
fn multi_member_adz_decompresses_every_member() -> Result<()> {
    // Concatenated gzip members are one valid gzip stream (`cat a.gz
    // b.gz`), and only the whole of it is the ADF: a decoder that stopped
    // at the first member would produce a short image the format dispatch
    // could report only as an unknown format.
    let mut adf = vec![0u8; ADF_SIZE];
    adf[ADF_SIZE - 4..].copy_from_slice(b"TAIL");
    let (first, second) = adf.split_at(ADF_SIZE / 2);
    let mut packed = gzip_bytes(first)?;
    packed.extend_from_slice(&gzip_bytes(second)?);

    let decoded = decode_gzip_floppy_image(&packed)?;
    assert_eq!(decoded.len(), ADF_SIZE);
    assert_eq!(&decoded[ADF_SIZE - 4..], b"TAIL");
    Ok(())
}

#[test]
fn adz_floppy_decompresses_standard_adf_as_read_only() -> Result<()> {
    let mut adf = vec![0u8; ADF_SIZE];
    for (idx, byte) in adf.iter_mut().take(BYTES_PER_SECTOR).enumerate() {
        *byte = idx as u8;
    }
    let path = temp_adz(&adf)?;
    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: 100,
        drives: [
            Some(FloppyDriveConfig {
                path: path.clone(),
                write_protected: false,
            }),
            None,
            None,
            None,
        ],
    };

    let ctrl = FloppyController::from_config(&cfg)?;
    let image = ctrl.drives[0].image.as_ref().unwrap();
    assert!(image.write_protected);
    match &image.data {
        FloppyImageData::StandardAdf(decoded) => assert_eq!(decoded, &adf),
        FloppyImageData::Tracks(_) => panic!("ADZ should decode to a standard ADF image"),
    }

    let _ = fs::remove_file(path);
    Ok(())
}

#[test]
fn gzip_floppy_can_wrap_uae_extended_adf_as_read_only() -> Result<()> {
    let raw_words = [0x4489, 0x2AAA, 0x5555, 0xA144];
    let ext_path = temp_ext2_raw(&raw_words)?;
    let ext_image = fs::read(&ext_path)?;
    let path = temp_gzip("test.ext.adf.gz", &ext_image)?;
    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: 100,
        drives: [
            Some(FloppyDriveConfig {
                path: path.clone(),
                write_protected: false,
            }),
            None,
            None,
            None,
        ],
    };
    let mut ctrl = FloppyController::from_config(&cfg)?;

    ctrl.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    ctrl.tick(MOTOR_READY_CCK, 0, &mut []);
    ctrl.ensure_track(0, 0);

    assert_eq!(ctrl.drives[0].cached_words(), raw_words);
    assert!(ctrl.drives[0]
        .image
        .as_ref()
        .is_some_and(|image| image.write_protected));

    let _ = fs::remove_file(ext_path);
    let _ = fs::remove_file(path);
    Ok(())
}

/// An IPF describes the written track rather than its sectors, so it
/// arrives as a raw MFM revolution and can never be written back to.
#[test]
fn ipf_images_load_as_raw_mfm_tracks_and_are_write_protected() -> Result<()> {
    let path = temp_path("test.ipf");
    fs::write(&path, crate::ipf::tests::amigados_ipf_image())?;
    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: 100,
        drives: [
            Some(FloppyDriveConfig {
                path: path.clone(),
                // An IPF overrides a writable configuration.
                write_protected: false,
            }),
            None,
            None,
            None,
        ],
    };

    let controller = FloppyController::from_config(&cfg)?;
    let image = controller.drives[0]
        .image
        .as_ref()
        .expect("the IPF should have loaded");
    assert!(image.write_protected);
    let FloppyImageData::Tracks(tracks) = &image.data else {
        panic!("an IPF should decode to per-track raw MFM, not a sector image");
    };
    let Some(FloppyTrackImage::RawMfm { bit_len, words, .. }) = &tracks[0] else {
        panic!("cylinder 0 head 0 should hold a raw MFM revolution");
    };
    assert_eq!(*bit_len, crate::ipf::tests::AMIGADOS_TRACK_BITS);
    assert_eq!(words.len(), (*bit_len as usize).div_ceil(16));
    // The track the fixture describes is the only formatted one.
    assert!(tracks[1..].iter().all(Option::is_none));

    let _ = fs::remove_file(path);
    Ok(())
}

/// A host with no filesystem inserts bytes rather than a path: the
/// browser build's picker, drop target and `?df0=` fetch all land in
/// `insert_disk_image_bytes`, which sniffs the signature exactly as the
/// filesystem loader does. IPF is the case worth pinning, because the
/// format arrives as raw MFM instead of sectors and the only thing that
/// ever knew it apart from an ADF was the CAPS signature.
#[test]
fn ipf_bytes_insert_without_a_filesystem_and_stay_write_protected() -> Result<()> {
    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: 100,
        drives: [None, None, None, None],
    };
    let mut controller = FloppyController::from_config(&cfg)?;
    controller.insert_disk_image_bytes(
        0,
        crate::ipf::tests::amigados_ipf_image(),
        PathBuf::from("promo.ipf"),
        true,
    )?;

    let image = controller.drives[0]
        .image
        .as_ref()
        .expect("the IPF bytes should have loaded");
    assert!(image.write_protected);
    let FloppyImageData::Tracks(tracks) = &image.data else {
        panic!("an IPF should decode to per-track raw MFM, not a sector image");
    };
    let Some(FloppyTrackImage::RawMfm { bit_len, .. }) = &tracks[0] else {
        panic!("cylinder 0 head 0 should hold a raw MFM revolution");
    };
    assert_eq!(*bit_len, crate::ipf::tests::AMIGADOS_TRACK_BITS);
    // The label a filesystem-less host passes is what the page shows.
    assert_eq!(
        controller.inserted_disk_name(0).as_deref(),
        Some("promo.ipf")
    );
    Ok(())
}

#[test]
fn inserted_disk_image_asserts_change_and_preserves_drive_mechanics() -> Result<()> {
    let first = temp_adf()?;
    let second = temp_adf()?;
    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: 100,
        drives: [
            Some(FloppyDriveConfig {
                path: first.clone(),
                write_protected: true,
            }),
            None,
            None,
            None,
        ],
    };
    let mut ctrl = FloppyController::from_config(&cfg)?;
    ctrl.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    ctrl.tick(MOTOR_READY_CCK, 0, &mut []);
    ctrl.drives[0].step(true);
    let cylinder = ctrl.drives[0].cylinder;
    let motor_cck = ctrl.drives[0].motor_cck;

    ctrl.insert_disk_image(0, second.clone(), true)?;

    let drive = &ctrl.drives[0];
    assert_eq!(drive.cylinder, cylinder);
    assert_eq!(drive.motor_cck, motor_cck);
    assert!(drive.motor_on);
    assert!(drive.disk_change);
    assert_eq!(
        drive.image.as_ref().map(|image| image.path.as_path()),
        Some(second.as_path())
    );

    let _ = fs::remove_file(first);
    let _ = fs::remove_file(second);
    Ok(())
}

#[test]
fn disk_change_line_settles_after_step_insert_and_eject() -> Result<()> {
    let first = temp_adf()?;
    let second = temp_adf()?;
    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: 100,
        drives: [
            Some(FloppyDriveConfig {
                path: first.clone(),
                write_protected: true,
            }),
            None,
            None,
            None,
        ],
    };
    let mut ctrl = FloppyController::from_config(&cfg)?;
    let selected = !CIAB_DSKSEL0;
    let inward_high = selected & !CIAB_DSKDIREC;
    ctrl.write_prb(inward_high);
    assert_eq!(ctrl.cia_a_status_bits() & CIAA_DSKCHANGE, 0);

    ctrl.write_prb(inward_high & !CIAB_DSKSTEP);
    assert_eq!(ctrl.cia_a_status_bits() & CIAA_DSKCHANGE, 0);
    ctrl.tick(DISK_STATUS_SETTLE_CCK, 0, &mut []);
    assert_ne!(ctrl.cia_a_status_bits() & CIAA_DSKCHANGE, 0);

    ctrl.insert_disk_image(0, second.clone(), true)?;
    assert_ne!(ctrl.cia_a_status_bits() & CIAA_DSKCHANGE, 0);
    ctrl.tick(DISK_STATUS_SETTLE_CCK, 0, &mut []);
    assert_eq!(ctrl.cia_a_status_bits() & CIAA_DSKCHANGE, 0);

    ctrl.write_prb(inward_high);
    ctrl.write_prb(inward_high & !CIAB_DSKSTEP);
    ctrl.tick(DISK_STATUS_SETTLE_CCK, 0, &mut []);
    assert_ne!(ctrl.cia_a_status_bits() & CIAA_DSKCHANGE, 0);

    ctrl.eject_disk_image(0)?;
    assert_ne!(ctrl.cia_a_status_bits() & CIAA_DSKCHANGE, 0);
    ctrl.tick(DISK_STATUS_SETTLE_CCK, 0, &mut []);
    assert_eq!(ctrl.cia_a_status_bits() & CIAA_DSKCHANGE, 0);

    let _ = fs::remove_file(first);
    let _ = fs::remove_file(second);
    Ok(())
}

#[test]
fn empty_internal_drive_keeps_disk_change_asserted_until_media_steps() -> Result<()> {
    let media = temp_adf()?;
    let mut ctrl = FloppyController::default();
    let selected = !CIAB_DSKSEL0;
    let inward_high = selected & !CIAB_DSKDIREC;

    ctrl.write_prb(inward_high);
    let status = ctrl.cia_a_status_bits();
    assert_eq!(status & CIAA_DSKCHANGE, 0);
    assert_ne!(status & CIAA_DSKRDY, 0);

    ctrl.write_prb(inward_high & !CIAB_DSKSTEP);
    ctrl.tick(DISK_STATUS_SETTLE_CCK, 0, &mut []);
    assert_eq!(ctrl.cia_a_status_bits() & CIAA_DSKCHANGE, 0);

    ctrl.insert_disk_image(0, media.clone(), true)?;
    assert_eq!(ctrl.cia_a_status_bits() & CIAA_DSKCHANGE, 0);

    ctrl.write_prb(inward_high);
    wait_step_floor(&mut ctrl);
    ctrl.write_prb(inward_high & !CIAB_DSKSTEP);
    ctrl.tick(DISK_STATUS_SETTLE_CCK, 0, &mut []);
    assert_ne!(ctrl.cia_a_status_bits() & CIAA_DSKCHANGE, 0);

    let _ = fs::remove_file(media);
    Ok(())
}

#[test]
fn connected_empty_external_drive_keeps_disk_change_asserted_through_step_poll() {
    let mut ctrl = FloppyController::default();
    ctrl.set_connected_drives([true, true, false, false]);

    let selected = drive_select_prb(1, false);
    let inward_high = selected & !CIAB_DSKDIREC;
    ctrl.write_prb(inward_high);
    let status = ctrl.cia_a_status_bits();
    assert_eq!(status & CIAA_DSKCHANGE, 0);
    assert_ne!(status & CIAA_DSKRDY, 0);

    ctrl.write_prb(inward_high & !CIAB_DSKSTEP);
    ctrl.tick(DISK_STATUS_SETTLE_CCK, 0, &mut []);
    assert_eq!(ctrl.cia_a_status_bits() & CIAA_DSKCHANGE, 0);
}

#[test]
fn write_protect_line_settles_after_inserted_disk_change() -> Result<()> {
    let protected = temp_adf()?;
    let writable = temp_adf()?;
    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: 100,
        drives: [
            Some(FloppyDriveConfig {
                path: protected.clone(),
                write_protected: true,
            }),
            None,
            None,
            None,
        ],
    };
    let mut ctrl = FloppyController::from_config(&cfg)?;
    ctrl.write_prb(!CIAB_DSKSEL0);
    assert_eq!(ctrl.cia_a_status_bits() & CIAA_DSKPROT, 0);

    ctrl.insert_disk_image(0, writable.clone(), false)?;
    assert_eq!(ctrl.cia_a_status_bits() & CIAA_DSKPROT, 0);
    ctrl.tick(DISK_STATUS_SETTLE_CCK, 0, &mut []);
    assert_ne!(ctrl.cia_a_status_bits() & CIAA_DSKPROT, 0);

    ctrl.insert_disk_image(0, protected.clone(), true)?;
    assert_ne!(ctrl.cia_a_status_bits() & CIAA_DSKPROT, 0);
    ctrl.tick(DISK_STATUS_SETTLE_CCK, 0, &mut []);
    assert_eq!(ctrl.cia_a_status_bits() & CIAA_DSKPROT, 0);

    let _ = fs::remove_file(protected);
    let _ = fs::remove_file(writable);
    Ok(())
}

#[test]
fn cia_status_reflects_write_protect_track0_and_ready() -> Result<()> {
    let path = temp_adf()?;
    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: 100,
        drives: [
            Some(FloppyDriveConfig {
                path: path.clone(),
                write_protected: true,
            }),
            None,
            None,
            None,
        ],
    };
    let mut ctrl = FloppyController::from_config(&cfg)?;
    ctrl.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0 & !CIAB_DSKSIDE);
    assert_eq!(ctrl.cia_a_status_bits() & CIAA_DSKPROT, 0);
    assert_eq!(ctrl.cia_a_status_bits() & CIAA_DSKTRACK0, 0);
    assert_ne!(ctrl.cia_a_status_bits() & CIAA_DSKRDY, 0);
    ctrl.tick(MOTOR_READY_CCK, 0, &mut []);
    assert_eq!(ctrl.cia_a_status_bits() & CIAA_DSKRDY, 0);
    let _ = fs::remove_file(path);
    Ok(())
}

#[test]
fn cia_status_ready_line_tracks_motor_spinup_and_off() -> Result<()> {
    let path = temp_adf()?;
    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: 100,
        drives: [
            Some(FloppyDriveConfig {
                path: path.clone(),
                write_protected: true,
            }),
            None,
            None,
            None,
        ],
    };
    let mut ctrl = FloppyController::from_config(&cfg)?;
    let selected_motor_on = !CIAB_DSKMOTOR & !CIAB_DSKSEL0;
    let selected_motor_off = !CIAB_DSKSEL0;

    ctrl.write_prb(selected_motor_on);
    assert_ne!(ctrl.cia_a_status_bits() & CIAA_DSKRDY, 0);
    ctrl.tick(MOTOR_READY_CCK - 1, 0, &mut []);
    assert_ne!(ctrl.cia_a_status_bits() & CIAA_DSKRDY, 0);
    ctrl.tick(1, 0, &mut []);
    assert_eq!(ctrl.cia_a_status_bits() & CIAA_DSKRDY, 0);

    // While the motor line is off the ready line drops immediately...
    ctrl.write_prb(0xFF);
    ctrl.write_prb(selected_motor_off);
    assert_ne!(ctrl.cia_a_status_bits() & CIAA_DSKRDY, 0);
    // ...but the platter keeps spinning (inertia), so a brief off/on
    // toggle with no elapsed time re-asserts ready without a respin.
    ctrl.write_prb(0xFF);
    ctrl.write_prb(selected_motor_on);
    assert_eq!(ctrl.cia_a_status_bits() & CIAA_DSKRDY, 0);

    // A sustained motor-off spins the platter down: once enough time has
    // elapsed the drive is no longer ready and must spin up again.
    ctrl.write_prb(0xFF);
    ctrl.write_prb(selected_motor_off);
    ctrl.tick(MOTOR_READY_CCK, 0, &mut []);
    ctrl.write_prb(0xFF);
    ctrl.write_prb(selected_motor_on);
    assert_ne!(ctrl.cia_a_status_bits() & CIAA_DSKRDY, 0);
    ctrl.tick(MOTOR_READY_CCK, 0, &mut []);
    assert_eq!(ctrl.cia_a_status_bits() & CIAA_DSKRDY, 0);

    let _ = fs::remove_file(path);
    Ok(())
}

#[test]
fn activity_led_follows_selected_drive_and_motor_line() {
    let mut ctrl = FloppyController::default();

    ctrl.write_prb(0xFF);
    assert!(!ctrl.activity_led_on());

    ctrl.write_prb(!CIAB_DSKSEL0);
    assert!(!ctrl.activity_led_on());

    ctrl.write_prb(0xFF);
    ctrl.write_prb(!CIAB_DSKSEL0 & !CIAB_DSKMOTOR);
    assert!(ctrl.activity_led_on());
}

#[test]
fn side_select_maps_lower_head_to_even_adf_tracks() -> Result<()> {
    let path = temp_adf()?;
    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: 100,
        drives: [
            Some(FloppyDriveConfig {
                path: path.clone(),
                write_protected: true,
            }),
            None,
            None,
            None,
        ],
    };
    let mut ctrl = FloppyController::from_config(&cfg)?;

    ctrl.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    assert_eq!(ctrl.track_for_drive(0), 0);
    assert_eq!(ctrl.selected_track(), Some(0));

    ctrl.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0 & !CIAB_DSKSIDE);
    assert_eq!(ctrl.track_for_drive(0), 1);
    assert_eq!(ctrl.selected_track(), Some(1));

    let _ = fs::remove_file(path);
    Ok(())
}

#[test]
fn selected_track_follows_step_and_side_select_lines() {
    let mut ctrl = FloppyController::default();
    let lower_head = !CIAB_DSKMOTOR & !CIAB_DSKSEL0;
    ctrl.write_prb(lower_head);
    assert_eq!(ctrl.selected_track(), Some(0));

    let inward_step_high = lower_head & !CIAB_DSKDIREC;
    ctrl.write_prb(inward_step_high);
    ctrl.write_prb(inward_step_high & !CIAB_DSKSTEP);
    assert_eq!(ctrl.selected_track(), Some(2));

    ctrl.write_prb(inward_step_high & !CIAB_DSKSTEP & !CIAB_DSKSIDE);
    assert_eq!(ctrl.selected_track(), Some(3));

    ctrl.write_prb(0xFF);
    assert_eq!(ctrl.selected_track(), None);
}

/// Advance a selected drive past the mechanism's step-pulse floor so
/// the next STEP edge is accepted (pulses are otherwise back-to-back in
/// emulated time, which the stepper ignores like real hardware does).
fn wait_step_floor(ctrl: &mut FloppyController) {
    ctrl.tick(MIN_STEP_PULSE_CCK as u32, 0, &mut []);
}

#[test]
fn step_pulses_move_head_on_each_falling_edge() {
    let mut ctrl = FloppyController::default();
    let lower_head = !CIAB_DSKMOTOR & !CIAB_DSKSEL0;
    let inward_high = lower_head & !CIAB_DSKDIREC;
    ctrl.write_prb(inward_high);

    ctrl.write_prb(inward_high & !CIAB_DSKSTEP);
    assert_eq!(ctrl.selected_track(), Some(2));

    ctrl.write_prb(inward_high);
    wait_step_floor(&mut ctrl);
    ctrl.write_prb(inward_high & !CIAB_DSKSTEP);
    assert_eq!(ctrl.selected_track(), Some(4));
}

#[test]
fn step_pulses_faster_than_the_mechanism_do_not_move_the_head() {
    let mut ctrl = FloppyController::default();
    let lower_head = !CIAB_DSKMOTOR & !CIAB_DSKSEL0;
    let inward_high = lower_head & !CIAB_DSKDIREC;
    ctrl.write_prb(inward_high);

    ctrl.write_prb(inward_high & !CIAB_DSKSTEP);
    assert_eq!(ctrl.selected_track(), Some(2));

    // A second pulse inside the ~40 us floor is ignored (vAmigaTS
    // Drive/step3: a too-fast burst leaves the head in place).
    ctrl.write_prb(inward_high);
    ctrl.write_prb(inward_high & !CIAB_DSKSTEP);
    assert_eq!(ctrl.selected_track(), Some(2));

    // After the floor elapses the next pulse steps again.
    ctrl.write_prb(inward_high);
    wait_step_floor(&mut ctrl);
    ctrl.write_prb(inward_high & !CIAB_DSKSTEP);
    assert_eq!(ctrl.selected_track(), Some(4));
}

#[test]
fn step_direction_reversal_moves_on_next_falling_edge() {
    let mut ctrl = FloppyController::default();
    let lower_head = !CIAB_DSKMOTOR & !CIAB_DSKSEL0;
    let inward_high = lower_head & !CIAB_DSKDIREC;
    let outward_high = lower_head | CIAB_DSKDIREC;
    ctrl.write_prb(inward_high);

    ctrl.write_prb(inward_high & !CIAB_DSKSTEP);
    assert_eq!(ctrl.selected_track(), Some(2));

    ctrl.write_prb(outward_high);
    wait_step_floor(&mut ctrl);
    ctrl.write_prb(outward_high & !CIAB_DSKSTEP);
    assert_eq!(ctrl.selected_track(), Some(0));
}

#[test]
fn outward_step_at_track_zero_is_gated_silent_by_trk0_sensor() {
    // Trackdisk NoClick patches poll the change line by pulsing STEP
    // outward with the head parked at cylinder 0. The /TRK0 sensor
    // gates the pulse before the stepper, so the head neither moves
    // nor clicks (issue #161).
    let mut ctrl = FloppyController::default();
    let selected = !CIAB_DSKMOTOR & !CIAB_DSKSEL0;
    let inward_high = selected & !CIAB_DSKDIREC;
    let outward_high = selected | CIAB_DSKDIREC;

    ctrl.write_prb(outward_high);
    ctrl.write_prb(outward_high & !CIAB_DSKSTEP);
    assert_eq!(ctrl.selected_track(), Some(0));
    assert_eq!(ctrl.take_sound_steps(), 0);

    // A pulse that does move the head is audible.
    ctrl.write_prb(inward_high);
    wait_step_floor(&mut ctrl);
    ctrl.write_prb(inward_high & !CIAB_DSKSTEP);
    assert_eq!(ctrl.selected_track(), Some(2));
    assert_eq!(ctrl.take_sound_steps(), 1);

    // Back at cylinder 0, an outward poll goes quiet again.
    ctrl.write_prb(outward_high);
    wait_step_floor(&mut ctrl);
    ctrl.write_prb(outward_high & !CIAB_DSKSTEP);
    assert_eq!(ctrl.selected_track(), Some(0));
    assert_eq!(ctrl.take_sound_steps(), 1);
    ctrl.write_prb(outward_high);
    wait_step_floor(&mut ctrl);
    ctrl.write_prb(outward_high & !CIAB_DSKSTEP);
    assert_eq!(ctrl.selected_track(), Some(0));
    assert_eq!(ctrl.take_sound_steps(), 0);
}

#[test]
fn inward_step_at_inner_clamp_still_clicks() {
    // There is no inner-limit sensor: an inward pulse with the head
    // at the last cylinder still fires the stepper and bangs the end
    // stop audibly, even though the head cannot move further.
    let mut ctrl = FloppyController::default();
    let selected = !CIAB_DSKMOTOR & !CIAB_DSKSEL0;
    let inward_high = selected & !CIAB_DSKDIREC;
    ctrl.write_prb(inward_high);
    for _ in 0..CYLINDERS + 2 {
        ctrl.write_prb(inward_high & !CIAB_DSKSTEP);
        ctrl.write_prb(inward_high);
        wait_step_floor(&mut ctrl);
    }
    assert_eq!(ctrl.selected_track(), Some((CYLINDERS as u8 - 1) * 2));
    ctrl.take_sound_steps();

    ctrl.write_prb(inward_high & !CIAB_DSKSTEP);
    assert_eq!(ctrl.selected_track(), Some((CYLINDERS as u8 - 1) * 2));
    assert_eq!(ctrl.take_sound_steps(), 1);
}

#[test]
fn step_pulse_swallowed_by_the_mechanism_is_silent() {
    // A second pulse inside the ~40 us mechanism floor does not move
    // the head (vAmigaTS Drive/step3) and produces no click either.
    let mut ctrl = FloppyController::default();
    let selected = !CIAB_DSKMOTOR & !CIAB_DSKSEL0;
    let inward_high = selected & !CIAB_DSKDIREC;
    ctrl.write_prb(inward_high);

    ctrl.write_prb(inward_high & !CIAB_DSKSTEP);
    assert_eq!(ctrl.take_sound_steps(), 1);

    ctrl.write_prb(inward_high);
    ctrl.write_prb(inward_high & !CIAB_DSKSTEP);
    assert_eq!(ctrl.selected_track(), Some(2));
    assert_eq!(ctrl.take_sound_steps(), 0);
}

#[test]
fn track_zero_line_follows_head_position() -> Result<()> {
    let path = temp_adf()?;
    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: 100,
        drives: [
            Some(FloppyDriveConfig {
                path: path.clone(),
                write_protected: true,
            }),
            None,
            None,
            None,
        ],
    };
    let mut ctrl = FloppyController::from_config(&cfg)?;
    let selected = !CIAB_DSKSEL0;
    let inward_high = selected & !CIAB_DSKDIREC;
    let outward_high = selected | CIAB_DSKDIREC;
    // Power-on at cylinder 0: /TRK0 asserted (active-low, bit clear).
    ctrl.write_prb(inward_high);
    assert_eq!(ctrl.cia_a_status_bits() & CIAA_DSKTRACK0, 0);

    // One inward step to cylinder 1 de-asserts /TRK0 immediately, with no
    // settle delay (the position sensor follows the head, not the data
    // settle).
    ctrl.write_prb(inward_high & !CIAB_DSKSTEP);
    assert_eq!(ctrl.selected_track(), Some(2));
    assert_ne!(ctrl.cia_a_status_bits() & CIAA_DSKTRACK0, 0);

    // Stepping back out to cylinder 0 re-asserts /TRK0 immediately.
    ctrl.write_prb(outward_high);
    wait_step_floor(&mut ctrl);
    ctrl.write_prb(outward_high & !CIAB_DSKSTEP);
    assert_eq!(ctrl.selected_track(), Some(0));
    assert_eq!(ctrl.cia_a_status_bits() & CIAA_DSKTRACK0, 0);

    let _ = fs::remove_file(path);
    Ok(())
}

#[test]
fn track_zero_asserts_immediately_during_rapid_recalibrate() {
    // A trackloader recalibrates by pulsing STEP outward and polling /TRK0
    // between pulses with no settle wait. /TRK0 must assert the moment the
    // head reaches cylinder 0, otherwise the loader steps past the stop or
    // hangs (the Magic Pockets recalibrate failure).
    let mut ctrl = FloppyController::default();
    let selected = !CIAB_DSKMOTOR & !CIAB_DSKSEL0;
    let inward_high = selected & !CIAB_DSKDIREC;
    let outward_high = selected | CIAB_DSKDIREC;

    // Seek inward 3 cylinders.
    for _ in 0..3 {
        ctrl.write_prb(inward_high);
        wait_step_floor(&mut ctrl);
        ctrl.write_prb(inward_high & !CIAB_DSKSTEP);
    }
    assert_eq!(ctrl.selected_track(), Some(6));
    assert_ne!(ctrl.cia_a_status_bits() & CIAA_DSKTRACK0, 0);

    // Fast outward recalibrate (legal pulse spacing, no settle wait);
    // /TRK0 is checked right at each step with no extra delay.
    ctrl.write_prb(outward_high);
    wait_step_floor(&mut ctrl);
    let mut asserted_at = None;
    for step in 1..=8 {
        ctrl.write_prb(outward_high & !CIAB_DSKSTEP);
        ctrl.write_prb(outward_high);
        if ctrl.cia_a_status_bits() & CIAA_DSKTRACK0 == 0 {
            asserted_at = Some(step);
            break;
        }
        wait_step_floor(&mut ctrl);
    }
    assert_eq!(asserted_at, Some(3));
    assert_eq!(ctrl.selected_track(), Some(0));
}

#[test]
fn side_select_crosses_cylinder_head_boundaries_after_steps() {
    let mut ctrl = FloppyController::default();
    let lower_head = !CIAB_DSKMOTOR & !CIAB_DSKSEL0;
    let inward_high = lower_head & !CIAB_DSKDIREC;
    let outward_high = lower_head | CIAB_DSKDIREC;

    ctrl.write_prb(inward_high);
    ctrl.write_prb(inward_high & !CIAB_DSKSTEP);
    assert_eq!(ctrl.selected_track(), Some(2));
    ctrl.write_prb((inward_high & !CIAB_DSKSTEP) & !CIAB_DSKSIDE);
    assert_eq!(ctrl.selected_track(), Some(3));

    ctrl.write_prb(outward_high & !CIAB_DSKSIDE);
    wait_step_floor(&mut ctrl);
    ctrl.write_prb((outward_high & !CIAB_DSKSIDE) & !CIAB_DSKSTEP);
    assert_eq!(ctrl.selected_track(), Some(1));
    ctrl.write_prb(outward_high);
    assert_eq!(ctrl.selected_track(), Some(0));
}

#[test]
fn cia_b_drive_select_lines_map_df0_bit3_to_df3_bit6() {
    let mut ctrl = FloppyController::default();

    for (idx, select_mask) in CIAB_DSKSEL_MASKS.iter().enumerate() {
        ctrl.write_prb(!select_mask);
        assert_eq!(ctrl.selected_drive(), Some(idx));
    }

    ctrl.write_prb(0xFF);
    assert_eq!(ctrl.selected_drive(), None);
}

#[test]
fn external_drive_id_reads_shift_msb_first_on_rdy() {
    let mut ctrl = FloppyController::default();
    ctrl.drives[1].external_id = 0xA5A5_0001;

    assert_eq!(read_external_drive_id(&mut ctrl, 1), 0xA5A5_0001);
    assert_eq!(ctrl.drives[1].external_id_bit, 32);
    assert!(!ctrl.drives[1].motor_on);
}

#[test]
fn external_drive_selects_follow_daisy_chain_order_for_df1_to_df3() {
    let mut ctrl = FloppyController::default();
    let ids = [0, 0x8000_0001, 0x4000_0002, 0x2000_0003];
    for idx in 1..=3 {
        ctrl.drives[idx].external_id = ids[idx];
    }

    for idx in 1..=3 {
        assert_eq!(read_external_drive_id(&mut ctrl, idx), ids[idx]);
        ctrl.write_prb(drive_select_prb(idx, false));
        assert_eq!(ctrl.selected_drive(), Some(idx));
        ctrl.write_prb(drive_deselect_prb(false));
    }
}

#[test]
fn unconfigured_external_drive_slots_do_not_answer_drive_id() {
    let mut ctrl = FloppyController::default();

    for idx in 1..ctrl.drives.len() {
        assert_eq!(read_external_drive_id(&mut ctrl, idx), 0);
    }
}

#[test]
fn configured_external_drive_defaults_to_standard_amiga_drive_id() -> Result<()> {
    let path = temp_adf()?;
    let mut ctrl = FloppyController::default();
    ctrl.drives[1] = FloppyDrive::load(&FloppyDriveConfig {
        path,
        write_protected: true,
    })?;

    assert_eq!(
        read_external_drive_id(&mut ctrl, 1),
        STANDARD_EXTERNAL_DRIVE_ID
    );
    Ok(())
}

#[test]
fn connected_empty_external_drive_answers_standard_drive_id() {
    let mut ctrl = FloppyController::default();
    ctrl.set_connected_drives([true, true, false, true]);

    assert!(ctrl.drive_connected(1));
    assert!(!ctrl.disk_inserted(1));
    assert_eq!(
        read_external_drive_id(&mut ctrl, 1),
        STANDARD_EXTERNAL_DRIVE_ID
    );
    assert!(!ctrl.drive_connected(2));
    assert_eq!(read_external_drive_id(&mut ctrl, 2), 0);
    assert!(ctrl.drive_connected(3));
    assert_eq!(
        read_external_drive_id(&mut ctrl, 3),
        STANDARD_EXTERNAL_DRIVE_ID
    );
}

#[test]
fn internal_df0_motor_follows_selected_motor_line_level() {
    let mut ctrl = FloppyController::default();

    ctrl.write_prb(drive_select_prb(0, true));
    assert!(ctrl.drives[0].motor_on);
    ctrl.write_prb(drive_select_prb(0, false));
    assert!(!ctrl.drives[0].motor_on);
    ctrl.write_prb(drive_select_prb(0, true));
    assert!(ctrl.drives[0].motor_on);
}

#[test]
fn external_drive_mtrxd_latches_only_on_select_active_edge() {
    let mut ctrl = FloppyController::default();
    let idx = 1;

    ctrl.write_prb(drive_select_prb(idx, false));
    assert!(!ctrl.drives[idx].motor_on);
    ctrl.write_prb(drive_select_prb(idx, true));
    assert!(!ctrl.drives[idx].motor_on);

    ctrl.write_prb(drive_deselect_prb(true));
    ctrl.write_prb(drive_select_prb(idx, true));
    assert!(ctrl.drives[idx].motor_on);
    ctrl.write_prb(drive_select_prb(idx, false));
    assert!(ctrl.drives[idx].motor_on);

    ctrl.write_prb(drive_deselect_prb(false));
    ctrl.write_prb(drive_select_prb(idx, false));
    assert!(!ctrl.drives[idx].motor_on);
}

#[test]
fn dresb_does_not_reset_internal_df0_motor_latch() {
    let mut ctrl = FloppyController::default();

    ctrl.write_prb(drive_select_prb(0, true));
    ctrl.tick(MOTOR_READY_CCK, 0, &mut []);
    assert!(ctrl.drives[0].motor_on);

    ctrl.reset_external_drives();

    assert!(ctrl.drives[0].motor_on);
}

#[test]
fn dresb_resets_external_motor_latch_and_write_protect_sense() {
    let mut ctrl = FloppyController::default();
    let idx = 1;

    ctrl.drives[idx].write_protected_target = false;
    ctrl.drives[idx].write_protected_sense = false;
    ctrl.write_prb(drive_select_prb(idx, true));
    ctrl.tick(MOTOR_READY_CCK, 0, &mut []);
    assert!(ctrl.drives[idx].motor_on);
    assert!(!ctrl.drives[idx].write_protected_sense);

    ctrl.reset_external_drives();
    assert!(!ctrl.drives[idx].motor_on);
    assert_eq!(ctrl.drives[idx].motor_cck, 0);
    assert!(ctrl.drives[idx].write_protected_sense);

    ctrl.write_prb(drive_select_prb(idx, false));
    assert_eq!(ctrl.cia_a_status_bits() & CIAA_DSKPROT, 0);
    ctrl.tick(DISK_STATUS_SETTLE_CCK, 0, &mut []);
    assert_ne!(ctrl.cia_a_status_bits() & CIAA_DSKPROT, 0);
}

#[test]
fn dskbytr_byte_valid_tracks_new_rotation_words() -> Result<()> {
    let path = temp_adf()?;
    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: 100,
        drives: [
            Some(FloppyDriveConfig {
                path: path.clone(),
                write_protected: true,
            }),
            None,
            None,
            None,
        ],
    };
    let mut ctrl = FloppyController::from_config(&cfg)?;
    ctrl.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    ctrl.tick(MOTOR_READY_CCK, 0, &mut []);

    ctrl.ensure_track(0, 0);
    let first_word = ctrl.drives[0].cached_words()[ctrl.drives[0].rotation_word_index()];
    ctrl.write_dsksync(first_word);

    let first = ctrl.read_dskbytr(0, 0);
    assert_ne!(first & DSKBYT, 0);
    assert_ne!(first & WORDEQUAL, 0);

    let second = ctrl.read_dskbytr(0, 0);
    assert_eq!(second & DSKBYT, 0);
    assert_ne!(second & WORDEQUAL, 0);

    ctrl.tick(ctrl.word_cck(), 0, &mut []);
    let third = ctrl.read_dskbytr(0, 0);
    assert_ne!(third & DSKBYT, 0);

    let _ = fs::remove_file(path);
    Ok(())
}

#[test]
fn dskbytr_reports_current_disk_byte_phase() -> Result<()> {
    let raw_words = [0x1234, 0xABCD];
    let path = temp_ext2_raw(&raw_words)?;
    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: 100,
        drives: [
            Some(FloppyDriveConfig {
                path: path.clone(),
                write_protected: true,
            }),
            None,
            None,
            None,
        ],
    };
    let mut ctrl = FloppyController::from_config(&cfg)?;

    ctrl.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    ctrl.tick(MOTOR_READY_CCK, 0, &mut []);
    ctrl.ensure_track(0, 0);
    ctrl.drives[0].set_rotation_word(0);
    ctrl.drives[0].rotation_acc_cck = 0;
    let word_cck = FloppyController::word_cck_for_track_words(raw_words.len());
    let half_word_cck = word_cck.div_ceil(2);

    let high = ctrl.read_dskbytr(0, 0);
    assert_ne!(high & DSKBYT, 0);
    assert_eq!(high & 0x00FF, 0x12);

    let repeat_high = ctrl.read_dskbytr(0, 0);
    assert_eq!(repeat_high & DSKBYT, 0);
    assert_eq!(repeat_high & 0x00FF, 0x12);

    ctrl.tick(half_word_cck, 0, &mut []);
    let low = ctrl.read_dskbytr(0, 0);
    assert_ne!(low & DSKBYT, 0);
    assert_eq!(low & 0x00FF, 0x34);

    ctrl.tick(word_cck - half_word_cck, 0, &mut []);
    let next_high = ctrl.read_dskbytr(0, 0);
    assert_ne!(next_high & DSKBYT, 0);
    assert_eq!(next_high & 0x00FF, 0xAB);

    let _ = fs::remove_file(path);
    Ok(())
}

#[test]
fn bit_stream_assembles_aligned_disk_bytes() {
    let words = [0x1234, 0xABCD];

    let high = DiskBitStream::from_word_phase(&words, words.len(), 0, 0).unwrap();
    assert_eq!(high.bit_position(), 0);
    assert_eq!(high.index_position(), 0);
    assert_eq!(high.storage_word_position(), 0);
    assert_eq!(high.storage_word(), 0x1234);
    assert_eq!(high.assembled_byte(), 0x12);

    let low = DiskBitStream::from_word_phase(&words, words.len(), 0, 8).unwrap();
    assert_eq!(low.bit_position(), 8);
    assert_eq!(low.index_position(), 8);
    assert_eq!(low.storage_word_position(), 0);
    assert_eq!(low.storage_word(), 0x1234);
    assert_eq!(low.assembled_byte(), 0x34);
}

#[test]
fn bit_stream_assembles_sub_word_disk_bytes() {
    let words = [0x1234, 0xABCD];

    let mid_word = DiskBitStream::from_word_phase(&words, words.len(), 0, 4).unwrap();
    assert_eq!(mid_word.bit_position(), 4);
    assert_eq!(mid_word.assembled_byte(), 0x23);

    let cross_word = DiskBitStream::from_word_phase(&words, words.len(), 0, 12).unwrap();
    assert_eq!(cross_word.bit_position(), 12);
    assert_eq!(cross_word.assembled_byte(), 0x4A);
}

#[test]
fn bit_stream_reports_index_relative_position() {
    let words = [0x1111, 0x2222, 0x3333, 0x4444];
    let stream = DiskBitStream::from_word_phase(&words, 2, 3, 5).unwrap();

    assert_eq!(stream.bit_position(), 53);
    assert_eq!(stream.index_position(), 21);
    assert_eq!(stream.storage_word_position(), 3);
    assert_eq!(stream.storage_word(), 0x4444);
}

#[test]
fn bit_stream_detects_sync_words_at_bit_phase() {
    let words = [0x1234, 0xABCD];

    let aligned = DiskBitStream::from_word_phase(&words, words.len(), 0, 0).unwrap();
    assert!(aligned.sync_matches(0x1234));
    assert!(!aligned.sync_matches(0x234A));

    let cross_word = DiskBitStream::from_word_phase(&words, words.len(), 0, 12).unwrap();
    assert!(cross_word.sync_matches(0x4ABC));
    assert_eq!(cross_word.assembled_word(), 0x4ABC);
}

#[test]
fn bit_stream_uses_rotation_bit_phase() {
    let words = [0x1234, 0xF0AA];
    let word_cck = 160;
    let stream =
        DiskBitStream::from_rotation(&words, words.len(), 1, word_cck / 4, word_cck).unwrap();

    assert_eq!(stream.bit_position(), 20);
    assert_eq!(stream.storage_word_position(), 1);
    assert_eq!(stream.assembled_byte(), 0x0A);
}

#[test]
fn dpll_fifo_shifts_disk_bytes_and_read_words() {
    let words = [0x1234, 0xABCD];
    let stream = DiskBitStream::from_word_phase(&words, words.len(), 0, 0).unwrap();
    let mut dpll = PaulaDiskReadDpllFifo::new();

    dpll.sample_stream_range(&stream, 0, 8, DEFAULT_DSKSYNC);
    let first_byte = dpll.read_dskbytr();
    assert_ne!(first_byte & DSKBYT, 0);
    assert_eq!(first_byte & 0x00FF, 0x12);
    assert_eq!(dpll.fifo_len(), 0);
    assert_eq!(dpll.read_dskbytr() & DSKBYT, 0);

    dpll.sample_stream_range(&stream, 8, 8, DEFAULT_DSKSYNC);
    let second_byte = dpll.read_dskbytr();
    assert_ne!(second_byte & DSKBYT, 0);
    assert_eq!(second_byte & 0x00FF, 0x34);
    assert_eq!(dpll.fifo_len(), 1);
    assert_eq!(dpll.read_fifo_word(), Some(0x1234));

    dpll.sample_stream_range(&stream, 16, 16, DEFAULT_DSKSYNC);
    assert_eq!(dpll.fifo_len(), 1);
    assert_eq!(dpll.read_fifo_word(), Some(0xABCD));
    assert_eq!(dpll.read_fifo_word(), None);
}

#[test]
fn dpll_fifo_detects_unaligned_disk_sync_word() {
    let words = [0x1234, 0xABCD];
    let stream = DiskBitStream::from_word_phase(&words, words.len(), 0, 4).unwrap();
    let mut dpll = PaulaDiskReadDpllFifo::new();

    dpll.sample_stream_range(&stream, 0, 8, 0x234A);
    let first_byte = dpll.read_dskbytr();
    assert_ne!(first_byte & DSKBYT, 0);
    assert_eq!(first_byte & 0x00FF, 0x23);
    assert_eq!(first_byte & WORDEQUAL, 0);
    assert!(!dpll.take_sync_irq());

    dpll.sample_stream_range(&stream, 8, 8, 0x234A);
    let second_byte = dpll.read_dskbytr();
    assert_ne!(second_byte & DSKBYT, 0);
    assert_eq!(second_byte & 0x00FF, 0x4A);
    assert_ne!(second_byte & WORDEQUAL, 0);
    assert!(dpll.take_sync_irq());
    assert_eq!(dpll.read_fifo_word(), Some(0x234A));

    dpll.sample_stream_range(&stream, 16, 1, 0x234A);
    assert_eq!(dpll.read_dskbytr() & WORDEQUAL, 0);
}

#[test]
fn dpll_fifo_preserves_oldest_words_when_full() {
    let words = [0x1111, 0x2222, 0x3333, 0x4444];
    let stream = DiskBitStream::from_word_phase(&words, words.len(), 0, 0).unwrap();
    let mut dpll = PaulaDiskReadDpllFifo::new();

    dpll.sample_stream_bits(&stream, words.len() * 16, DEFAULT_DSKSYNC);

    assert_eq!(dpll.fifo_len(), 3);
    assert!(dpll.fifo_overflowed());
    assert_eq!(dpll.read_fifo_word(), Some(0x1111));
    assert_eq!(dpll.read_fifo_word(), Some(0x2222));
    assert_eq!(dpll.read_fifo_word(), Some(0x3333));
    assert_eq!(dpll.read_fifo_word(), None);
}

#[test]
fn dpll_fifo_dskbytr_read_clears_byte_ready_only() {
    let words = [DEFAULT_DSKSYNC, 0x2222];
    let stream = DiskBitStream::from_word_phase(&words, words.len(), 0, 0).unwrap();
    let mut dpll = PaulaDiskReadDpllFifo::new();

    dpll.sample_stream_bits(&stream, 16, DEFAULT_DSKSYNC);

    let first = dpll.read_dskbytr();
    assert_ne!(first & DSKBYT, 0);
    assert_ne!(first & WORDEQUAL, 0);
    assert_eq!(first & 0x00FF, 0x89);
    assert!(dpll.take_sync_irq());

    let repeat = dpll.read_dskbytr();
    assert_eq!(repeat & DSKBYT, 0);
    assert_ne!(repeat & WORDEQUAL, 0);
    assert_eq!(repeat & 0x00FF, 0x89);
}

#[test]
fn dskbytr_wordequal_tracks_current_word() -> Result<()> {
    let raw_words = [0x1234, 0x5678];
    let path = temp_ext2_raw(&raw_words)?;
    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: 100,
        drives: [
            Some(FloppyDriveConfig {
                path: path.clone(),
                write_protected: true,
            }),
            None,
            None,
            None,
        ],
    };
    let mut ctrl = FloppyController::from_config(&cfg)?;

    ctrl.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    ctrl.tick(MOTOR_READY_CCK, 0, &mut []);
    ctrl.ensure_track(0, 0);
    ctrl.drives[0].set_rotation_word(0);
    ctrl.drives[0].rotation_acc_cck = 0;
    ctrl.write_dsksync(raw_words[0]);

    let first = ctrl.read_dskbytr(0, 0);
    assert_ne!(first & WORDEQUAL, 0);
    let repeat = ctrl.read_dskbytr(0, 0);
    assert_ne!(repeat & WORDEQUAL, 0);

    ctrl.tick(
        FloppyController::word_cck_for_track_words(raw_words.len()),
        0,
        &mut [],
    );
    let next = ctrl.read_dskbytr(0, 0);
    assert_eq!(next & WORDEQUAL, 0);

    let _ = fs::remove_file(path);
    Ok(())
}

#[test]
fn msbsync_dskbytr_keeps_wordequal_without_stream_irq() -> Result<()> {
    let raw_words = [DEFAULT_DSKSYNC, 0x5678];
    let path = temp_ext2_raw(&raw_words)?;
    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: 100,
        drives: [
            Some(FloppyDriveConfig {
                path: path.clone(),
                write_protected: true,
            }),
            None,
            None,
            None,
        ],
    };
    let mut ctrl = FloppyController::from_config(&cfg)?;

    ctrl.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    ctrl.tick(MOTOR_READY_CCK, 0, &mut []);
    ctrl.ensure_track(0, 0);
    ctrl.drives[0].set_rotation_word(0);
    ctrl.drives[0].rotation_acc_cck = 0;

    let first = ctrl.read_dskbytr(0, ADK_MSBSYNC);
    assert_ne!(first & DSKBYT, 0);
    assert_ne!(first & WORDEQUAL, 0);
    assert!(!ctrl.take_sync_irq());

    let _ = fs::remove_file(path);
    Ok(())
}

#[test]
fn write_dma_dskbytr_keeps_wordequal_without_stream_irq() -> Result<()> {
    let raw_words = [DEFAULT_DSKSYNC, 0x5678];
    let path = temp_ext2_raw(&raw_words)?;
    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: 100,
        drives: [
            Some(FloppyDriveConfig {
                path: path.clone(),
                write_protected: true,
            }),
            None,
            None,
            None,
        ],
    };
    let mut ctrl = FloppyController::from_config(&cfg)?;

    ctrl.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    ctrl.tick(MOTOR_READY_CCK, 0, &mut []);
    ctrl.ensure_track(0, 0);
    ctrl.drives[0].set_rotation_word(0);
    ctrl.drives[0].rotation_acc_cck = 0;

    let len = DSKLEN_DMAEN | DSKLEN_WRITE | 1;
    assert!(!ctrl.write_dsklen(len, 0));
    assert!(!ctrl.write_dsklen(len, 0));

    let status = ctrl.read_dskbytr(DMACON_DMAEN | DMACON_DISK, 0);
    assert_ne!(status & DSKBYT, 0);
    assert_ne!(status & DMAON, 0);
    assert_ne!(status & DISKWRITE, 0);
    assert_ne!(status & WORDEQUAL, 0);
    assert_eq!(status & 0x00FF, 0x00);
    assert!(!ctrl.take_sync_irq());

    let _ = fs::remove_file(path);
    Ok(())
}

#[test]
fn dskbytr_write_mode_without_dma_suppresses_byte_loads() -> Result<()> {
    let raw_words = [0x1234, DEFAULT_DSKSYNC];
    let path = temp_ext2_raw(&raw_words)?;
    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: 100,
        drives: [
            Some(FloppyDriveConfig {
                path: path.clone(),
                write_protected: true,
            }),
            None,
            None,
            None,
        ],
    };
    let mut ctrl = FloppyController::from_config(&cfg)?;

    ctrl.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    ctrl.tick(MOTOR_READY_CCK, 0, &mut []);
    ctrl.ensure_track(0, 0);
    ctrl.drives[0].set_rotation_word(0);
    ctrl.drives[0].rotation_acc_cck = 0;

    let first = ctrl.read_dskbytr(0, 0);
    assert_ne!(first & DSKBYT, 0);
    assert_eq!(first & 0x00FF, 0x12);

    assert!(!ctrl.write_dsksync(DEFAULT_DSKSYNC));
    assert!(!ctrl.write_dsklen(DSKLEN_WRITE, 0));
    let word_cck = FloppyController::word_cck_for_track_words(raw_words.len());
    ctrl.tick(word_cck, 0, &mut []);

    let write_mode = ctrl.read_dskbytr(0, 0);
    assert_ne!(write_mode & DISKWRITE, 0);
    assert_eq!(write_mode & DSKBYT, 0);
    assert_ne!(write_mode & WORDEQUAL, 0);
    assert_eq!(write_mode & 0x00FF, 0x12);
    assert!(ctrl.take_sync_irq());

    assert!(!ctrl.write_dsklen(0, 0));
    let resumed = ctrl.read_dskbytr(0, 0);
    assert_ne!(resumed & DSKBYT, 0);
    assert_eq!(resumed & 0x00FF, 0x44);

    let _ = fs::remove_file(path);
    Ok(())
}

#[test]
fn dskbytr_dmaon_waits_for_double_dsklen_arm() -> Result<()> {
    let raw_words = [0x1234, 0x5678];
    let path = temp_ext2_raw(&raw_words)?;
    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: 100,
        drives: [
            Some(FloppyDriveConfig {
                path: path.clone(),
                write_protected: true,
            }),
            None,
            None,
            None,
        ],
    };
    let mut ctrl = FloppyController::from_config(&cfg)?;

    ctrl.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    ctrl.tick(MOTOR_READY_CCK, 0, &mut []);

    let len = DSKLEN_DMAEN | 1;
    let dmacon = DMACON_DMAEN | DMACON_DISK;
    assert!(!ctrl.write_dsklen(len, 0));
    assert_eq!(ctrl.read_dskbytr(dmacon, 0) & DMAON, 0);

    assert!(!ctrl.write_dsklen(len, 0));
    assert_ne!(ctrl.read_dskbytr(dmacon, 0) & DMAON, 0);
    assert_eq!(ctrl.read_dskbytr(0, 0) & DMAON, 0);

    let _ = fs::remove_file(path);
    Ok(())
}

#[test]
fn dskbytr_dmaon_stays_set_while_armed_dma_waits_for_the_motor() -> Result<()> {
    let raw_words = [0x1234, 0x5678];
    let path = temp_ext2_raw(&raw_words)?;
    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: 100,
        drives: [
            Some(FloppyDriveConfig {
                path: path.clone(),
                write_protected: true,
            }),
            None,
            None,
            None,
        ],
    };
    let mut ctrl = FloppyController::from_config(&cfg)?;

    // Selected drive with media but the motor line off: the transfer arms
    // and pends -- real Paula waits for data forever -- so DMAON stays set
    // and nothing completes until the platter turns.
    ctrl.write_prb(!CIAB_DSKSEL0);

    let len = DSKLEN_DMAEN | 1;
    let dmacon = DMACON_DMAEN | DMACON_DISK;
    assert!(!ctrl.write_dsklen(len, 0));
    assert!(!ctrl.write_dsklen(len, 0));
    assert_ne!(ctrl.read_dskbytr(dmacon, 0) & DMAON, 0);
    let mut chip_ram = vec![0u8; 4];
    assert!(!ctrl.tick(MOTOR_READY_CCK * 4, dmacon, &mut chip_ram));
    assert_eq!(read_chip_word(&chip_ram, 0), 0);

    let _ = fs::remove_file(path);
    Ok(())
}

#[test]
fn read_dma_with_no_media_arms_and_pends_until_media_arrives() -> Result<()> {
    let raw_words = [0x1234, 0x5678];
    let path = temp_ext2_raw(&raw_words)?;
    let mut ctrl = FloppyController::default();
    let mut chip_ram = vec![0u8; 4];

    // DF0 selected, motor spinning, but the bay is empty: the transfer
    // arms and idles -- no cells pass under the head, so no completion
    // interrupt ever fires and the guest's own timeout governs.
    ctrl.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    ctrl.tick(MOTOR_READY_CCK, 0, &mut []);
    let len = DSKLEN_DMAEN | 1;
    let dmacon = DMACON_DMAEN | DMACON_DISK;
    assert!(!ctrl.write_dsklen(len, 0));
    assert!(!ctrl.write_dsklen(len, 0));
    assert_ne!(ctrl.read_dskbytr(dmacon, 0) & DMAON, 0);

    // Several revolutions of waiting deliver nothing.
    for _ in 0..8 {
        assert!(!ctrl.tick(
            FloppyController::word_cck_for_track_words(raw_words.len()) * 125,
            dmacon,
            &mut chip_ram
        ));
    }
    assert_eq!(read_chip_word(&chip_ram, 0), 0);

    // Inserting media mid-transfer brings it to life exactly as sliding a
    // disk into a real drive would.
    ctrl.insert_disk_image(0, path.clone(), true)?;
    assert!(ctrl.tick(
        FloppyController::word_cck_for_track_words(raw_words.len()),
        dmacon,
        &mut chip_ram
    ));
    assert_eq!(read_chip_word(&chip_ram, 0), raw_words[0]);

    let _ = fs::remove_file(path);
    Ok(())
}

#[test]
fn read_dma_with_motor_off_pends_until_the_motor_starts() -> Result<()> {
    let raw_words = [0x1234, 0x5678];
    let path = temp_ext2_raw(&raw_words)?;
    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: 100,
        drives: [
            Some(FloppyDriveConfig {
                path: path.clone(),
                write_protected: true,
            }),
            None,
            None,
            None,
        ],
    };
    let mut ctrl = FloppyController::from_config(&cfg)?;
    let mut chip_ram = vec![0u8; 4];
    let len = DSKLEN_DMAEN | 1;
    let dmacon = DMACON_DMAEN | DMACON_DISK;

    // Media present, drive selected, motor line off: the read arms and
    // pends through several spin-up windows without completing.
    ctrl.write_prb(!CIAB_DSKSEL0);
    assert!(!ctrl.write_dsklen(len, 0));
    assert!(!ctrl.write_dsklen(len, 0));
    assert!(!ctrl.tick(MOTOR_READY_CCK * 3, dmacon, &mut chip_ram));
    assert_eq!(read_chip_word(&chip_ram, 0), 0);

    // Starting the motor spins the platter up and the pending transfer
    // completes with the track data.
    ctrl.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    let word_cck = FloppyController::word_cck_for_track_words(raw_words.len());
    let mut completed = false;
    for _ in 0..(MOTOR_READY_CCK / word_cck + 4) {
        if ctrl.tick(word_cck, dmacon, &mut chip_ram) {
            completed = true;
            break;
        }
    }
    assert!(completed, "the transfer completes after the motor starts");
    assert_eq!(read_chip_word(&chip_ram, 0), raw_words[0]);

    let _ = fs::remove_file(path);
    Ok(())
}

#[test]
fn read_dma_started_during_motor_spinup_arms_and_transfers() -> Result<()> {
    let raw_words = [0x1234, 0x5678];
    let path = temp_ext2_raw(&raw_words)?;
    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: 100,
        drives: [
            Some(FloppyDriveConfig {
                path: path.clone(),
                write_protected: true,
            }),
            None,
            None,
            None,
        ],
    };
    let mut ctrl = FloppyController::from_config(&cfg)?;
    let mut chip_ram = vec![0u8; 4];

    // Motor just latched on: the drive is spinning up (/RDY still high),
    // but Paula's DSKLEN arming does not sense readiness, so the
    // double-write enters the read state and the transfer proceeds.
    ctrl.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    assert_ne!(ctrl.cia_a_status_bits() & CIAA_DSKRDY, 0);
    ctrl.set_dskpt_low(0);
    let len = DSKLEN_DMAEN | 1;
    assert!(!ctrl.write_dsklen(len, 0));
    assert!(!ctrl.write_dsklen(len, 0));
    let dmacon = DMACON_DMAEN | DMACON_DISK;

    assert_ne!(ctrl.read_dskbytr(dmacon, 0) & DMAON, 0);
    assert!(ctrl.tick(
        FloppyController::word_cck_for_track_words(raw_words.len()),
        dmacon,
        &mut chip_ram
    ));
    assert_eq!(read_chip_word(&chip_ram, 0), raw_words[0]);

    let _ = fs::remove_file(path);
    Ok(())
}

/// Controller with one raw ext-ADF track in DF0 at the given `[floppy]
/// speed`, spun up with the head pinned to word 0, ready to arm a DMA.
fn spun_up_speed_controller(raw_words: &[u16], speed: u16) -> Result<(FloppyController, PathBuf)> {
    let path = temp_ext2_raw(raw_words)?;
    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed,
        drives: [
            Some(FloppyDriveConfig {
                path: path.clone(),
                write_protected: true,
            }),
            None,
            None,
            None,
        ],
    };
    let mut ctrl = FloppyController::from_config(&cfg)?;
    ctrl.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    ctrl.tick(MOTOR_READY_CCK, 0, &mut []);
    ctrl.ensure_track(0, 0);
    ctrl.drives[0].set_rotation_word(0);
    ctrl.drives[0].rotation_acc_cck = 0;
    ctrl.set_dskpt_low(0);
    Ok((ctrl, path))
}

#[test]
fn floppy_speed_800_compresses_read_dma_eightfold_with_identical_data() -> Result<()> {
    let raw_words = [0x1111, 0x2222, 0x3333, 0x4444];
    let word_cck = FloppyController::word_cck_for_track_words(raw_words.len());
    let dmacon = DMACON_DMAEN | DMACON_DISK;
    let mut data = [[0u16; 3]; 2];
    for (run, speed) in [100u16, 800].into_iter().enumerate() {
        let (mut ctrl, path) = spun_up_speed_controller(&raw_words, speed)?;
        let mut chip_ram = vec![0u8; 8];
        let len = DSKLEN_DMAEN | 3;
        assert!(!ctrl.write_dsklen(len, 0));
        assert!(!ctrl.write_dsklen(len, 0));
        // The whole data path is scaled, so the transfer lands the
        // multiple sooner to the exact cck, and the completion
        // prediction reports the scaled deadline.
        let scaled = 3 * word_cck / u32::from(speed / 100);
        assert_eq!(ctrl.next_completion_cck(dmacon), Some(scaled));
        assert!(!ctrl.tick(scaled - 1, dmacon, &mut chip_ram));
        assert!(ctrl.tick(1, dmacon, &mut chip_ram));
        data[run] = [
            read_chip_word(&chip_ram, 0),
            read_chip_word(&chip_ram, 2),
            read_chip_word(&chip_ram, 4),
        ];
        let _ = fs::remove_file(path);
    }
    // Faster, but bit-identical: both speeds deliver the same words.
    assert_eq!(data[0], data[1]);
    assert_eq!(data[0], [0x1111, 0x2222, 0x3333]);
    Ok(())
}

#[test]
fn floppy_turbo_bursts_read_dma_after_two_line_grace() -> Result<()> {
    // ~177000 cck per word on this track: real pacing moves well under
    // one word inside the grace window, so a completion there can only
    // come from the burst.
    let raw_words = [0x1111, 0x2222, 0x3333, 0x4444];
    let dmacon = DMACON_DMAEN | DMACON_DISK;
    let (mut ctrl, path) = spun_up_speed_controller(&raw_words, SPEED_TURBO)?;
    let mut chip_ram = vec![0u8; 8];
    let len = DSKLEN_DMAEN | 3;
    assert!(!ctrl.write_dsklen(len, 0));
    assert!(!ctrl.write_dsklen(len, 0));
    // Inside the deferral window the transfer paces normally...
    assert!(!ctrl.tick(TURBO_DMA_GRACE_CCK - 10, dmacon, &mut chip_ram));
    // ...and the tick that crosses it bursts the transfer to the end.
    assert!(ctrl.tick(10, dmacon, &mut chip_ram));
    assert_eq!(read_chip_word(&chip_ram, 0), 0x1111);
    assert_eq!(read_chip_word(&chip_ram, 2), 0x2222);
    assert_eq!(read_chip_word(&chip_ram, 4), 0x3333);
    let _ = fs::remove_file(path);
    Ok(())
}

#[test]
fn floppy_turbo_sync_wait_matches_real_speed_data() -> Result<()> {
    // A sync-waiting read: the burst must first spin to the DSKSYNC
    // match, then drain the transfer, delivering exactly the words a
    // real-speed run recovers.
    let raw_words = [0x1111, DEFAULT_DSKSYNC, 0x2222, 0x3333];
    let word_cck = FloppyController::word_cck_for_track_words(raw_words.len());
    let dmacon = DMACON_DMAEN | DMACON_DISK;
    let mut data = [[0u16; 2]; 2];
    for (run, speed) in [100u16, SPEED_TURBO].into_iter().enumerate() {
        let (mut ctrl, path) = spun_up_speed_controller(&raw_words, speed)?;
        let mut chip_ram = vec![0u8; 8];
        ctrl.write_dsksync(DEFAULT_DSKSYNC);
        let len = DSKLEN_DMAEN | 2;
        assert!(!ctrl.write_dsklen(len, ADK_WORDSYNC));
        assert!(!ctrl.write_dsklen(len, ADK_WORDSYNC));
        let mut done = false;
        for _ in 0..8 {
            if ctrl.tick(word_cck, dmacon, &mut chip_ram) {
                done = true;
                break;
            }
        }
        assert!(done, "sync-wait DMA should complete at speed {speed}");
        assert!(ctrl.take_sync_irq());
        data[run] = [read_chip_word(&chip_ram, 0), read_chip_word(&chip_ram, 2)];
        let _ = fs::remove_file(path);
    }
    assert_eq!(data[0], data[1]);
    Ok(())
}

#[test]
fn floppy_turbo_missing_sync_leaves_dma_to_normal_pacing() -> Result<()> {
    // No DSKSYNC word anywhere on the track: the burst gets one look,
    // gives up, and the armed transfer keeps waiting at normal pace
    // (forever, like real hardware) instead of rescanning every tick.
    let raw_words = [0x1111, 0x2222, 0x3333, 0x4444];
    let word_cck = FloppyController::word_cck_for_track_words(raw_words.len());
    let dmacon = DMACON_DMAEN | DMACON_DISK;
    let (mut ctrl, path) = spun_up_speed_controller(&raw_words, SPEED_TURBO)?;
    let mut chip_ram = vec![0u8; 8];
    ctrl.write_dsksync(DEFAULT_DSKSYNC);
    let len = DSKLEN_DMAEN | 2;
    assert!(!ctrl.write_dsklen(len, ADK_WORDSYNC));
    assert!(!ctrl.write_dsklen(len, ADK_WORDSYNC));
    for _ in 0..8 {
        assert!(!ctrl.tick(word_cck, dmacon, &mut chip_ram));
    }
    assert!(ctrl.dma.is_some());
    assert!(ctrl.turbo_burst_spent);
    assert_eq!(read_chip_word(&chip_ram, 0), 0);
    let _ = fs::remove_file(path);
    Ok(())
}

#[test]
fn floppy_live_turbo_toggle_mid_dma_honors_grace() -> Result<()> {
    // Flipping the menu to turbo while a transfer is in flight must
    // defer the burst by the same two-scanline grace as a fresh arming,
    // not complete on the very next tick.
    let raw_words = [0x1111, 0x2222, 0x3333, 0x4444];
    let word_cck = FloppyController::word_cck_for_track_words(raw_words.len());
    let dmacon = DMACON_DMAEN | DMACON_DISK;
    let (mut ctrl, path) = spun_up_speed_controller(&raw_words, 100)?;
    let mut chip_ram = vec![0u8; 8];
    let len = DSKLEN_DMAEN | 3;
    assert!(!ctrl.write_dsklen(len, 0));
    assert!(!ctrl.write_dsklen(len, 0));
    // One word into the transfer, switch to turbo live.
    assert!(!ctrl.tick(word_cck, dmacon, &mut chip_ram));
    ctrl.set_speed_percent(SPEED_TURBO);
    // A stale spent flag must not survive the toggle either.
    assert!(!ctrl.turbo_burst_spent);
    assert!(!ctrl.tick(TURBO_DMA_GRACE_CCK - 10, dmacon, &mut chip_ram));
    assert!(ctrl.tick(10, dmacon, &mut chip_ram));
    assert_eq!(read_chip_word(&chip_ram, 4), 0x3333);
    let _ = fs::remove_file(path);
    Ok(())
}

#[test]
fn floppy_turbo_prediction_paces_normally_while_drive_not_ready() -> Result<()> {
    // With the grace elapsed but the drive still spinning up, the burst
    // cannot run; the completion prediction must fall back to the
    // normal-paced deadline instead of forcing 1-cck idle stepping for
    // the rest of the spin-up.
    let raw_words = [0x1111, 0x2222, 0x3333, 0x4444];
    let path = temp_ext2_raw(&raw_words)?;
    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: SPEED_TURBO,
        drives: [
            Some(FloppyDriveConfig {
                path: path.clone(),
                write_protected: true,
            }),
            None,
            None,
            None,
        ],
    };
    let mut ctrl = FloppyController::from_config(&cfg)?;
    let mut chip_ram = vec![0u8; 8];
    let dmacon = DMACON_DMAEN | DMACON_DISK;
    ctrl.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    ctrl.set_dskpt_low(0);
    let len = DSKLEN_DMAEN | 1;
    assert!(!ctrl.write_dsklen(len, 0));
    assert!(!ctrl.write_dsklen(len, 0));
    // Inside the grace the prediction is bounded by it.
    assert!(ctrl.next_completion_cck(dmacon).unwrap() <= TURBO_DMA_GRACE_CCK);
    // Grace elapsed, motor still spinning up: normal-paced deadline.
    assert!(!ctrl.tick(TURBO_DMA_GRACE_CCK, dmacon, &mut chip_ram));
    assert!(ctrl.next_completion_cck(dmacon).unwrap() > TURBO_DMA_GRACE_CCK);
    // Once ready the burst is imminent again.
    ctrl.tick(MOTOR_READY_CCK, 0, &mut chip_ram);
    assert_eq!(ctrl.next_completion_cck(dmacon), Some(1));
    let _ = fs::remove_file(path);
    Ok(())
}

#[test]
fn floppy_turbo_burst_waits_for_motor_spinup() -> Result<()> {
    let raw_words = [0x1111, 0x2222, 0x3333, 0x4444];
    let path = temp_ext2_raw(&raw_words)?;
    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: SPEED_TURBO,
        drives: [
            Some(FloppyDriveConfig {
                path: path.clone(),
                write_protected: true,
            }),
            None,
            None,
            None,
        ],
    };
    let mut ctrl = FloppyController::from_config(&cfg)?;
    let mut chip_ram = vec![0u8; 8];
    let dmacon = DMACON_DMAEN | DMACON_DISK;
    // Motor just latched on: the DMA arms (spin-up arming is modelled),
    // but turbo must not deliver data before the mechanism is ready.
    ctrl.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    ctrl.set_dskpt_low(0);
    let len = DSKLEN_DMAEN | 1;
    assert!(!ctrl.write_dsklen(len, 0));
    assert!(!ctrl.write_dsklen(len, 0));
    assert!(!ctrl.tick(TURBO_DMA_GRACE_CCK, dmacon, &mut chip_ram));
    assert!(ctrl.dma.is_some());
    // Spin-up completes within this tick; the burst follows in the
    // same tick and finishes the transfer.
    assert!(ctrl.tick(MOTOR_READY_CCK, dmacon, &mut chip_ram));
    let _ = fs::remove_file(path);
    Ok(())
}

#[test]
fn floppy_speed_never_accelerates_motor_spinup() -> Result<()> {
    for speed in [800u16, SPEED_TURBO] {
        let raw_words = [0x1234, 0x5678];
        let path = temp_ext2_raw(&raw_words)?;
        let cfg = FloppyConfig {
            bridges: std::array::from_fn(|_| None),
            speed,
            drives: [
                Some(FloppyDriveConfig {
                    path: path.clone(),
                    write_protected: true,
                }),
                None,
                None,
                None,
            ],
        };
        let mut ctrl = FloppyController::from_config(&cfg)?;
        ctrl.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
        ctrl.tick(MOTOR_READY_CCK - 1, 0, &mut []);
        assert_ne!(
            ctrl.cia_a_status_bits() & CIAA_DSKRDY,
            0,
            "speed {speed} must not shorten spin-up"
        );
        ctrl.tick(1, 0, &mut []);
        assert_eq!(ctrl.cia_a_status_bits() & CIAA_DSKRDY, 0);
        let _ = fs::remove_file(path);
    }
    Ok(())
}

#[test]
fn motor_off_blocks_read_dma_until_next_spinup() -> Result<()> {
    let raw_words = [0x1234, 0x5678];
    let path = temp_ext2_raw(&raw_words)?;
    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: 100,
        drives: [
            Some(FloppyDriveConfig {
                path: path.clone(),
                write_protected: true,
            }),
            None,
            None,
            None,
        ],
    };
    let mut ctrl = FloppyController::from_config(&cfg)?;
    let mut chip_ram = vec![0u8; 4];
    let selected_motor_on = !CIAB_DSKMOTOR & !CIAB_DSKSEL0;
    let selected_motor_off = !CIAB_DSKSEL0;

    ctrl.write_prb(selected_motor_on);
    ctrl.tick(MOTOR_READY_CCK, 0, &mut chip_ram);
    assert_eq!(ctrl.cia_a_status_bits() & CIAA_DSKRDY, 0);
    ctrl.write_prb(0xFF);
    ctrl.write_prb(selected_motor_off);
    assert_ne!(ctrl.cia_a_status_bits() & CIAA_DSKRDY, 0);

    ctrl.set_dskpt_low(0);
    let len = DSKLEN_DMAEN | 1;
    let dmacon = DMACON_DMAEN | DMACON_DISK;
    // Arming against the stopped platter enters the transfer state and
    // pends: no cells pass under the head, so nothing completes.
    assert!(!ctrl.write_dsklen(len, 0));
    assert!(!ctrl.write_dsklen(len, 0));
    assert_ne!(ctrl.read_dskbytr(dmacon, 0) & DMAON, 0);

    // Leave the motor off long enough for the platter to spin down fully,
    // so the drive must spin back up before data can flow.
    ctrl.tick(MOTOR_READY_CCK, 0, &mut chip_ram);
    assert!(!ctrl.tick(MOTOR_READY_CCK, dmacon, &mut chip_ram));
    ctrl.write_prb(0xFF);
    ctrl.write_prb(selected_motor_on);
    assert_ne!(ctrl.cia_a_status_bits() & CIAA_DSKRDY, 0);
    ctrl.tick(MOTOR_READY_CCK, 0, &mut chip_ram);
    assert_eq!(ctrl.cia_a_status_bits() & CIAA_DSKRDY, 0);

    // The pending transfer completes once the platter is turning again.
    let word_cck = FloppyController::word_cck_for_track_words(raw_words.len());
    let mut completed = false;
    for _ in 0..(MOTOR_READY_CCK / word_cck + 4) {
        if ctrl.tick(word_cck, dmacon, &mut chip_ram) {
            completed = true;
            break;
        }
    }
    assert!(completed, "the transfer completes after the spin-up");
    // The platter kept its rotation position across the motor-off window,
    // so the pending read resumes wherever the head now is on the track.
    let word = read_chip_word(&chip_ram, 0);
    assert!(
        raw_words.contains(&word),
        "resumed read delivers real track data, got {word:#06x}"
    );

    let _ = fs::remove_file(path);
    Ok(())
}

#[test]
fn dsksync_write_latches_current_word_match() -> Result<()> {
    let path = temp_adf()?;
    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: 100,
        drives: [
            Some(FloppyDriveConfig {
                path: path.clone(),
                write_protected: true,
            }),
            None,
            None,
            None,
        ],
    };
    let mut ctrl = FloppyController::from_config(&cfg)?;
    ctrl.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    ctrl.tick(MOTOR_READY_CCK, 0, &mut []);
    ctrl.ensure_track(0, 0);
    let current_word = ctrl.drives[0].cached_words()[ctrl.drives[0].rotation_word_index()];

    assert!(ctrl.write_dsksync(current_word));
    assert!(ctrl.take_sync_irq());

    let _ = fs::remove_file(path);
    Ok(())
}

#[test]
fn reset_dsksync_defaults_to_amigados_mfm_sync_word() -> Result<()> {
    let raw_words = [DEFAULT_DSKSYNC, 0x2222];
    let path = temp_ext2_raw(&raw_words)?;
    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: 100,
        drives: [
            Some(FloppyDriveConfig {
                path: path.clone(),
                write_protected: true,
            }),
            None,
            None,
            None,
        ],
    };
    let mut ctrl = FloppyController::from_config(&cfg)?;

    ctrl.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    ctrl.tick(MOTOR_READY_CCK, 0, &mut []);
    ctrl.ensure_track(0, 0);
    ctrl.drives[0].set_rotation_word(0);
    ctrl.drives[0].rotation_acc_cck = 0;

    let status = ctrl.read_dskbytr(0, 0);
    assert_ne!(status & WORDEQUAL, 0);
    assert!(ctrl.take_sync_irq());

    let _ = fs::remove_file(path);
    Ok(())
}

#[test]
fn read_dma_sync_irq_does_not_require_wordsync() -> Result<()> {
    let path = temp_adf()?;
    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: 100,
        drives: [
            Some(FloppyDriveConfig {
                path: path.clone(),
                write_protected: true,
            }),
            None,
            None,
            None,
        ],
    };
    let mut ctrl = FloppyController::from_config(&cfg)?;
    let mut chip_ram = vec![0u8; 8];
    ctrl.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    ctrl.tick(MOTOR_READY_CCK, 0, &mut chip_ram);
    ctrl.ensure_track(0, 0);
    let sync_pos = ctrl.drives[0]
        .cached_words()
        .iter()
        .position(|&word| word == DEFAULT_DSKSYNC)
        .unwrap();
    ctrl.drives[0].set_rotation_word(sync_pos);
    assert!(ctrl.write_dsksync(DEFAULT_DSKSYNC));
    assert!(ctrl.take_sync_irq());

    ctrl.set_dskpt_low(0);
    let len = DSKLEN_DMAEN | 1;
    assert!(!ctrl.write_dsklen(len, 0));
    assert!(!ctrl.write_dsklen(len, 0));
    assert!(ctrl.tick(ctrl.word_cck(), DMACON_DMAEN | DMACON_DISK, &mut chip_ram));

    assert!(ctrl.take_sync_irq());
    assert_eq!(read_chip_word(&chip_ram, 0), DEFAULT_DSKSYNC);

    let _ = fs::remove_file(path);
    Ok(())
}

#[test]
fn wordsync_skips_initial_sync_then_transfers_repeated_sync_word() -> Result<()> {
    let path = temp_adf()?;
    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: 100,
        drives: [
            Some(FloppyDriveConfig {
                path: path.clone(),
                write_protected: true,
            }),
            None,
            None,
            None,
        ],
    };
    let mut ctrl = FloppyController::from_config(&cfg)?;
    let mut chip_ram = vec![0u8; 8];
    ctrl.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    ctrl.tick(MOTOR_READY_CCK, 0, &mut chip_ram);
    assert!(!ctrl.write_dsksync(DEFAULT_DSKSYNC));
    ctrl.ensure_track(0, 0);
    let sync_pos = ctrl.drives[0]
        .cached_words()
        .iter()
        .position(|&word| word == DEFAULT_DSKSYNC)
        .unwrap();
    ctrl.drives[0].set_rotation_word(sync_pos);

    ctrl.set_dskpt_low(0);
    let len = DSKLEN_DMAEN | 1;
    assert!(!ctrl.write_dsklen(len, ADK_WORDSYNC));
    assert!(!ctrl.write_dsklen(len, ADK_WORDSYNC));
    let dmacon = DMACON_DMAEN | DMACON_DISK;

    assert!(!ctrl.tick(ctrl.word_cck(), dmacon, &mut chip_ram));
    assert_eq!(read_chip_word(&chip_ram, 0), 0);

    assert!(ctrl.take_sync_irq());
    assert!(ctrl.tick(ctrl.word_cck(), dmacon, &mut chip_ram));
    assert_eq!(read_chip_word(&chip_ram, 0), DEFAULT_DSKSYNC);

    let _ = fs::remove_file(path);
    Ok(())
}

#[test]
fn wordsync_locks_onto_bit_aligned_sync_and_frames_following_word() -> Result<()> {
    // 0x4489 straddles a word boundary, occupying bits 8..24: word0's low
    // byte is 0x44 and word1's high byte is 0x89. A word-grid scan never
    // sees it; the bit-level shifter locks on at bit 23 and frames the
    // first transferred word from bit 24 (word1 low byte 0x55 + word2 high
    // byte 0x12 = 0x5512).
    let raw_words = [0xAA44u16, 0x8955, 0x1234, 0x5678, 0x0000];
    let path = temp_ext2_raw(&raw_words)?;
    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: 100,
        drives: [
            Some(FloppyDriveConfig {
                path: path.clone(),
                write_protected: true,
            }),
            None,
            None,
            None,
        ],
    };
    let mut ctrl = FloppyController::from_config(&cfg)?;
    let mut chip_ram = vec![0u8; 8];
    ctrl.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    ctrl.tick(MOTOR_READY_CCK, 0, &mut chip_ram);
    ctrl.ensure_track(0, 0);
    ctrl.drives[0].set_rotation_bit(0);
    ctrl.drives[0].rotation_acc_cck = 0;

    ctrl.set_dskpt_low(0);
    let len = DSKLEN_DMAEN | 1;
    assert!(!ctrl.write_dsklen(len, ADK_WORDSYNC));
    assert!(!ctrl.write_dsklen(len, ADK_WORDSYNC));
    let dmacon = DMACON_DMAEN | DMACON_DISK;
    let word_cck = FloppyController::word_cck_for_track_words(raw_words.len());

    let mut done = false;
    for _ in 0..5 {
        if ctrl.tick(word_cck, dmacon, &mut chip_ram) {
            done = true;
            break;
        }
    }
    assert!(done, "bit-aligned sync-wait DMA should complete");
    assert!(ctrl.take_sync_irq());
    assert_eq!(read_chip_word(&chip_ram, 0), 0x5512);

    let _ = fs::remove_file(path);
    Ok(())
}

#[test]
fn read_dma_sync_irq_deadline_tracks_next_sync_word() -> Result<()> {
    let raw_words = [0x1111, DEFAULT_DSKSYNC, 0x2222];
    let path = temp_ext2_raw(&raw_words)?;
    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: 100,
        drives: [
            Some(FloppyDriveConfig {
                path: path.clone(),
                write_protected: true,
            }),
            None,
            None,
            None,
        ],
    };
    let mut ctrl = FloppyController::from_config(&cfg)?;
    let mut chip_ram = vec![0u8; 8];
    ctrl.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    ctrl.tick(MOTOR_READY_CCK, 0, &mut chip_ram);
    ctrl.ensure_track(0, 0);
    ctrl.drives[0].set_rotation_word(0);
    ctrl.drives[0].rotation_acc_cck = 0;

    ctrl.set_dskpt_low(0);
    let len = DSKLEN_DMAEN | raw_words.len() as u16;
    assert!(!ctrl.write_dsklen(len, 0));
    assert!(!ctrl.write_dsklen(len, 0));
    let dmacon = DMACON_DMAEN | DMACON_DISK;
    let word_cck = FloppyController::word_cck_for_track_words(raw_words.len());

    assert_eq!(ctrl.next_completion_cck(dmacon), Some(3 * word_cck));
    assert_eq!(ctrl.next_sync_irq_cck(dmacon), Some(2 * word_cck));

    assert!(!ctrl.tick(word_cck - 1, dmacon, &mut chip_ram));
    assert_eq!(ctrl.next_sync_irq_cck(dmacon), Some(word_cck + 1));
    assert!(!ctrl.take_sync_irq());

    assert!(!ctrl.tick(1, dmacon, &mut chip_ram));
    assert_eq!(ctrl.next_sync_irq_cck(dmacon), Some(word_cck));
    assert!(!ctrl.take_sync_irq());

    assert!(!ctrl.tick(word_cck, dmacon, &mut chip_ram));
    assert!(ctrl.take_sync_irq());
    assert_eq!(read_chip_word(&chip_ram, 2), DEFAULT_DSKSYNC);

    let _ = fs::remove_file(path);
    Ok(())
}

#[test]
fn active_read_dma_dsksync_change_updates_dskbytr_wordequal() -> Result<()> {
    let raw_words = [0x1111, 0x2222, 0x3333];
    let path = temp_ext2_raw(&raw_words)?;
    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: 100,
        drives: [
            Some(FloppyDriveConfig {
                path: path.clone(),
                write_protected: true,
            }),
            None,
            None,
            None,
        ],
    };
    let mut ctrl = FloppyController::from_config(&cfg)?;
    let mut chip_ram = vec![0u8; 8];
    ctrl.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    ctrl.tick(MOTOR_READY_CCK, 0, &mut chip_ram);
    ctrl.ensure_track(0, 0);
    ctrl.drives[0].set_rotation_word(0);
    ctrl.drives[0].rotation_acc_cck = 0;
    assert!(!ctrl.write_dsksync(0xFFFF));

    ctrl.set_dskpt_low(0);
    let len = DSKLEN_DMAEN | raw_words.len() as u16;
    assert!(!ctrl.write_dsklen(len, 0));
    assert!(!ctrl.write_dsklen(len, 0));
    let dmacon = DMACON_DMAEN | DMACON_DISK;
    let word_cck = FloppyController::word_cck_for_track_words(raw_words.len());

    assert!(!ctrl.tick(word_cck, dmacon, &mut chip_ram));
    assert_eq!(read_chip_word(&chip_ram, 0), raw_words[0]);
    assert!(!ctrl.take_sync_irq());

    assert!(ctrl.write_dsksync(raw_words[1]));
    let status = ctrl.read_dskbytr(dmacon, 0);
    assert_ne!(status & DMAON, 0);
    assert_ne!(status & WORDEQUAL, 0);
    assert!(ctrl.take_sync_irq());

    let _ = fs::remove_file(path);
    Ok(())
}

#[test]
fn active_read_dma_dsklen_rewrite_updates_remaining_length() -> Result<()> {
    let raw_words = [0x1111, 0x2222, 0x3333, 0x4444];
    let path = temp_ext2_raw(&raw_words)?;
    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: 100,
        drives: [
            Some(FloppyDriveConfig {
                path: path.clone(),
                write_protected: true,
            }),
            None,
            None,
            None,
        ],
    };
    let mut ctrl = FloppyController::from_config(&cfg)?;
    let mut chip_ram = vec![0u8; 8];
    ctrl.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    ctrl.tick(MOTOR_READY_CCK, 0, &mut chip_ram);
    ctrl.ensure_track(0, 0);
    ctrl.drives[0].set_rotation_word(0);
    ctrl.drives[0].rotation_acc_cck = 0;

    ctrl.set_dskpt_low(0);
    let len = DSKLEN_DMAEN | raw_words.len() as u16;
    assert!(!ctrl.write_dsklen(len, 0));
    assert!(!ctrl.write_dsklen(len, 0));
    let dmacon = DMACON_DMAEN | DMACON_DISK;
    let word_cck = FloppyController::word_cck_for_track_words(raw_words.len());

    assert!(!ctrl.tick(word_cck, dmacon, &mut chip_ram));
    assert_eq!(read_chip_word(&chip_ram, 0), 0x1111);

    assert!(!ctrl.write_dsklen(DSKLEN_DMAEN | 1, 0));
    assert_eq!(ctrl.next_completion_cck(dmacon), Some(word_cck));
    assert!(ctrl.tick(word_cck, dmacon, &mut chip_ram));

    assert_eq!(read_chip_word(&chip_ram, 0), 0x1111);
    assert_eq!(read_chip_word(&chip_ram, 2), 0x2222);
    assert_eq!(read_chip_word(&chip_ram, 4), 0x0000);

    let _ = fs::remove_file(path);
    Ok(())
}

#[test]
fn active_read_dma_dsklen_zero_rewrite_finishes_now() -> Result<()> {
    let raw_words = [0x1111, 0x2222, 0x3333, 0x4444];
    let path = temp_ext2_raw(&raw_words)?;
    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: 100,
        drives: [
            Some(FloppyDriveConfig {
                path: path.clone(),
                write_protected: true,
            }),
            None,
            None,
            None,
        ],
    };
    let mut ctrl = FloppyController::from_config(&cfg)?;
    let mut chip_ram = vec![0u8; 8];
    ctrl.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    ctrl.tick(MOTOR_READY_CCK, 0, &mut chip_ram);
    ctrl.ensure_track(0, 0);
    ctrl.drives[0].set_rotation_word(0);
    ctrl.drives[0].rotation_acc_cck = 0;

    ctrl.set_dskpt_low(0);
    let len = DSKLEN_DMAEN | 2;
    assert!(!ctrl.write_dsklen(len, 0));
    assert!(!ctrl.write_dsklen(len, 0));
    let dmacon = DMACON_DMAEN | DMACON_DISK;
    let word_cck = FloppyController::word_cck_for_track_words(raw_words.len());

    assert!(ctrl.write_dsklen(DSKLEN_DMAEN, 0));
    assert_eq!(ctrl.next_completion_cck(dmacon), None);
    assert!(!ctrl.tick(word_cck, dmacon, &mut chip_ram));
    assert_eq!(read_chip_word(&chip_ram, 0), 0x0000);

    let _ = fs::remove_file(path);
    Ok(())
}

#[test]
fn active_read_dma_follows_head_step_to_new_track() -> Result<()> {
    let track0 = [0x1111, 0x2222];
    let track2 = [0xAAAA, 0xBBBB];
    let path = temp_ext2_raw_tracks(&[(0, &track0), (2, &track2)])?;
    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: 100,
        drives: [
            Some(FloppyDriveConfig {
                path: path.clone(),
                write_protected: true,
            }),
            None,
            None,
            None,
        ],
    };
    let mut ctrl = FloppyController::from_config(&cfg)?;
    let mut chip_ram = vec![0u8; 8];

    let selected = !CIAB_DSKMOTOR & !CIAB_DSKSEL0;
    ctrl.write_prb(selected);
    ctrl.tick(MOTOR_READY_CCK, 0, &mut chip_ram);
    ctrl.ensure_track(0, 0);
    ctrl.drives[0].set_rotation_word(0);
    ctrl.drives[0].rotation_acc_cck = 0;
    ctrl.set_dskpt_low(0);
    let len = DSKLEN_DMAEN | 3;
    assert!(!ctrl.write_dsklen(len, 0));
    assert!(!ctrl.write_dsklen(len, 0));
    let dmacon = DMACON_DMAEN | DMACON_DISK;
    let word_cck = FloppyController::word_cck_for_track_words(track0.len());

    assert!(!ctrl.tick(word_cck, dmacon, &mut chip_ram));
    assert_eq!(read_chip_word(&chip_ram, 0), 0x1111);

    let step_high = selected & !CIAB_DSKDIREC;
    ctrl.write_prb(step_high);
    ctrl.write_prb(step_high & !CIAB_DSKSTEP);
    assert_eq!(ctrl.track_for_drive(0), 2);

    assert!(!ctrl.tick(word_cck, dmacon, &mut chip_ram));
    assert_eq!(read_chip_word(&chip_ram, 2), 0xBBBB);
    assert!(ctrl.tick(word_cck, dmacon, &mut chip_ram));
    assert_eq!(read_chip_word(&chip_ram, 4), 0xAAAA);

    let _ = fs::remove_file(path);
    Ok(())
}

// A read issued while the head is still settling after a step recovers no
// data (the cells under the moving head are garbage), while the platter
// keeps spinning -- so the read resumes a rotation-latency later, modelling
// a real drive's post-seek settle. /TRK0 and the cylinder index stay instant.
#[test]
fn read_dma_suppressed_during_post_seek_settle() -> Result<()> {
    let track0 = [0x1111u16, 0x2222, 0x3333, 0x4444];
    let path = temp_ext2_raw_tracks(&[(0, &track0)])?;
    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: 100,
        drives: [
            Some(FloppyDriveConfig {
                path: path.clone(),
                write_protected: true,
            }),
            None,
            None,
            None,
        ],
    };
    let mut ctrl = FloppyController::from_config(&cfg)?;
    let mut chip_ram = vec![0u8; 16];

    let selected = !CIAB_DSKMOTOR & !CIAB_DSKSEL0;
    ctrl.write_prb(selected);
    ctrl.tick(MOTOR_READY_CCK, 0, &mut chip_ram);
    ctrl.ensure_track(0, 0);
    ctrl.drives[0].set_rotation_word(0);
    ctrl.drives[0].rotation_acc_cck = 0;
    ctrl.set_dskpt_low(0);
    let len = DSKLEN_DMAEN | 4;
    assert!(!ctrl.write_dsklen(len, 0));
    assert!(!ctrl.write_dsklen(len, 0));
    let dmacon = DMACON_DMAEN | DMACON_DISK;
    let word_cck = FloppyController::word_cck_for_track_words(track0.len());

    // First word reads normally.
    assert!(!ctrl.tick(word_cck, dmacon, &mut chip_ram));
    assert_eq!(read_chip_word(&chip_ram, 0), 0x1111);

    // Force a settle window spanning the next two ticks: while it is active
    // the DMA makes no progress even though the head keeps rotating.
    ctrl.drives[0].seek_settle_cck = word_cck * 3;
    assert!(!ctrl.tick(word_cck, dmacon, &mut chip_ram));
    assert_eq!(read_chip_word(&chip_ram, 2), 0x0000);
    assert!(!ctrl.tick(word_cck, dmacon, &mut chip_ram));
    assert_eq!(read_chip_word(&chip_ram, 2), 0x0000);

    // Settle elapsed: the read resumes and recovers a (rotated) track word.
    assert!(!ctrl.tick(word_cck, dmacon, &mut chip_ram));
    assert_ne!(read_chip_word(&chip_ram, 2), 0x0000);

    let _ = fs::remove_file(path);
    Ok(())
}

#[test]
fn wordsync_read_dma_zero_rewrite_finishes_at_sync() -> Result<()> {
    let raw_words = [0x1111, DEFAULT_DSKSYNC, 0x2222];
    let path = temp_ext2_raw(&raw_words)?;
    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: 100,
        drives: [
            Some(FloppyDriveConfig {
                path: path.clone(),
                write_protected: true,
            }),
            None,
            None,
            None,
        ],
    };
    let mut ctrl = FloppyController::from_config(&cfg)?;
    let mut chip_ram = vec![0u8; 8];
    ctrl.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    ctrl.tick(MOTOR_READY_CCK, 0, &mut chip_ram);
    ctrl.ensure_track(0, 0);
    ctrl.drives[0].set_rotation_word(0);
    ctrl.drives[0].rotation_acc_cck = 0;

    ctrl.set_dskpt_low(0);
    let len = DSKLEN_DMAEN | 2;
    assert!(!ctrl.write_dsklen(len, ADK_WORDSYNC));
    assert!(!ctrl.write_dsklen(len, ADK_WORDSYNC));
    let dmacon = DMACON_DMAEN | DMACON_DISK;
    let word_cck = FloppyController::word_cck_for_track_words(raw_words.len());

    assert!(!ctrl.write_dsklen(DSKLEN_DMAEN, ADK_WORDSYNC));
    assert_eq!(ctrl.next_completion_cck(dmacon), None);
    assert!(!ctrl.tick(word_cck, dmacon, &mut chip_ram));
    assert_eq!(read_chip_word(&chip_ram, 0), 0x0000);
    assert!(ctrl.tick(word_cck, dmacon, &mut chip_ram));
    assert!(ctrl.take_sync_irq());
    assert_eq!(read_chip_word(&chip_ram, 0), 0x0000);

    let _ = fs::remove_file(path);
    Ok(())
}

#[test]
fn wordsync_wait_reports_sync_deadline_without_completion_deadline() -> Result<()> {
    let raw_words = [0x1111, DEFAULT_DSKSYNC, 0x2222];
    let path = temp_ext2_raw(&raw_words)?;
    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: 100,
        drives: [
            Some(FloppyDriveConfig {
                path: path.clone(),
                write_protected: true,
            }),
            None,
            None,
            None,
        ],
    };
    let mut ctrl = FloppyController::from_config(&cfg)?;
    let mut chip_ram = vec![0u8; 8];
    ctrl.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    ctrl.tick(MOTOR_READY_CCK, 0, &mut chip_ram);
    ctrl.ensure_track(0, 0);
    ctrl.drives[0].set_rotation_word(0);
    ctrl.drives[0].rotation_acc_cck = 0;

    ctrl.set_dskpt_low(0);
    let len = DSKLEN_DMAEN | 1;
    assert!(!ctrl.write_dsklen(len, ADK_WORDSYNC));
    assert!(!ctrl.write_dsklen(len, ADK_WORDSYNC));
    let dmacon = DMACON_DMAEN | DMACON_DISK;
    let word_cck = FloppyController::word_cck_for_track_words(raw_words.len());

    assert_eq!(ctrl.next_completion_cck(dmacon), None);
    assert_eq!(ctrl.next_sync_irq_cck(dmacon), Some(2 * word_cck));

    let _ = fs::remove_file(path);
    Ok(())
}

#[test]
fn msbsync_read_dma_suppresses_stream_sync_irq_deadline() -> Result<()> {
    let raw_words = [0x1111, DEFAULT_DSKSYNC, 0x2222];
    let path = temp_ext2_raw(&raw_words)?;
    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: 100,
        drives: [
            Some(FloppyDriveConfig {
                path: path.clone(),
                write_protected: true,
            }),
            None,
            None,
            None,
        ],
    };
    let mut ctrl = FloppyController::from_config(&cfg)?;
    let mut chip_ram = vec![0u8; 8];
    ctrl.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    ctrl.tick(MOTOR_READY_CCK, 0, &mut chip_ram);
    ctrl.ensure_track(0, 0);
    ctrl.drives[0].set_rotation_word(0);
    ctrl.drives[0].rotation_acc_cck = 0;

    ctrl.set_dskpt_low(0);
    let len = DSKLEN_DMAEN | raw_words.len() as u16;
    assert!(!ctrl.write_dsklen(len, ADK_MSBSYNC));
    assert!(!ctrl.write_dsklen(len, ADK_MSBSYNC));
    let dmacon = DMACON_DMAEN | DMACON_DISK;
    let word_cck = FloppyController::word_cck_for_track_words(raw_words.len());

    assert_eq!(ctrl.next_completion_cck(dmacon), Some(3 * word_cck));
    assert_eq!(ctrl.next_sync_irq_cck(dmacon), None);

    assert!(!ctrl.tick(word_cck, dmacon, &mut chip_ram));
    assert!(!ctrl.tick(word_cck, dmacon, &mut chip_ram));
    assert!(!ctrl.take_sync_irq());
    assert_eq!(read_chip_word(&chip_ram, 2), DEFAULT_DSKSYNC);
    assert!(ctrl.tick(word_cck, dmacon, &mut chip_ram));
    assert!(!ctrl.take_sync_irq());

    let _ = fs::remove_file(path);
    Ok(())
}

#[test]
fn msbsync_wordsync_wait_ignores_dsksync_word_match() -> Result<()> {
    let raw_words = [DEFAULT_DSKSYNC, 0x2222];
    let path = temp_ext2_raw(&raw_words)?;
    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: 100,
        drives: [
            Some(FloppyDriveConfig {
                path: path.clone(),
                write_protected: true,
            }),
            None,
            None,
            None,
        ],
    };
    let mut ctrl = FloppyController::from_config(&cfg)?;
    let mut chip_ram = vec![0u8; 4];
    ctrl.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    ctrl.tick(MOTOR_READY_CCK, 0, &mut chip_ram);
    ctrl.ensure_track(0, 0);
    ctrl.drives[0].set_rotation_word(0);
    ctrl.drives[0].rotation_acc_cck = 0;

    ctrl.set_dskpt_low(0);
    let len = DSKLEN_DMAEN | 1;
    let adkcon = ADK_WORDSYNC | ADK_MSBSYNC;
    assert!(!ctrl.write_dsklen(len, adkcon));
    assert!(!ctrl.write_dsklen(len, adkcon));
    let dmacon = DMACON_DMAEN | DMACON_DISK;
    let word_cck = FloppyController::word_cck_for_track_words(raw_words.len());

    assert_eq!(ctrl.next_completion_cck(dmacon), None);
    assert_eq!(ctrl.next_sync_irq_cck(dmacon), None);
    assert!(!ctrl.tick(word_cck, dmacon, &mut chip_ram));
    assert!(!ctrl.take_sync_irq());
    assert_eq!(read_chip_word(&chip_ram, 0), 0);
    assert_eq!(ctrl.next_completion_cck(dmacon), None);

    let _ = fs::remove_file(path);
    Ok(())
}

#[test]
fn read_dma_completion_deadline_preserves_sub_word_elapsed_time() -> Result<()> {
    let raw_words = [0x1111, 0x2222];
    let path = temp_ext2_raw(&raw_words)?;
    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: 100,
        drives: [
            Some(FloppyDriveConfig {
                path: path.clone(),
                write_protected: true,
            }),
            None,
            None,
            None,
        ],
    };
    let mut ctrl = FloppyController::from_config(&cfg)?;
    let mut chip_ram = vec![0u8; 4];
    ctrl.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    ctrl.tick(MOTOR_READY_CCK, 0, &mut chip_ram);
    ctrl.ensure_track(0, 0);
    ctrl.drives[0].set_rotation_word(0);
    ctrl.drives[0].rotation_acc_cck = 0;

    ctrl.set_dskpt_low(0);
    let len = DSKLEN_DMAEN | 1;
    assert!(!ctrl.write_dsklen(len, 0));
    assert!(!ctrl.write_dsklen(len, 0));
    let dmacon = DMACON_DMAEN | DMACON_DISK;
    let word_cck = FloppyController::word_cck_for_track_words(raw_words.len());

    assert!(!ctrl.tick(word_cck - 2, dmacon, &mut chip_ram));
    assert_eq!(ctrl.next_completion_cck(dmacon), Some(2));
    assert!(!ctrl.tick(1, dmacon, &mut chip_ram));
    assert_eq!(ctrl.next_completion_cck(dmacon), Some(1));
    assert!(ctrl.tick(1, dmacon, &mut chip_ram));

    let _ = fs::remove_file(path);
    Ok(())
}

#[test]
fn selected_drive_index_pulse_latches_once_per_wrap() -> Result<()> {
    let path = temp_adf()?;
    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: 100,
        drives: [
            Some(FloppyDriveConfig {
                path: path.clone(),
                write_protected: true,
            }),
            None,
            None,
            None,
        ],
    };
    let mut ctrl = FloppyController::from_config(&cfg)?;
    ctrl.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    ctrl.tick(MOTOR_READY_CCK, 0, &mut []);
    clear_index_flag(&mut ctrl);

    ctrl.drives[0].set_rotation_word(encoded_track_words() - 1);
    ctrl.drives[0].rotation_acc_cck = 0;
    ctrl.tick(ctrl.word_cck(), 0, &mut []);
    assert!(ctrl.index_pulse_active());
    assert!(!ctrl.take_index_pulse());
    assert_eq!(ctrl.next_index_pulse_cck(), Some(INDEX_FLAG_SYNC_CCK));
    tick_index_flag_sync(&mut ctrl);
    assert!(ctrl.take_index_pulse());
    assert!(!ctrl.take_index_pulse());

    let _ = fs::remove_file(path);
    Ok(())
}

#[test]
fn selected_drive_index_pulse_has_fixed_width() -> Result<()> {
    let path = temp_adf()?;
    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: 100,
        drives: [
            Some(FloppyDriveConfig {
                path: path.clone(),
                write_protected: true,
            }),
            None,
            None,
            None,
        ],
    };
    let mut ctrl = FloppyController::from_config(&cfg)?;
    ctrl.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    ctrl.tick(MOTOR_READY_CCK, 0, &mut []);
    clear_index_flag(&mut ctrl);

    ctrl.drives[0].set_rotation_word(encoded_track_words() - 1);
    ctrl.drives[0].rotation_acc_cck = 0;
    ctrl.tick(ctrl.word_cck(), 0, &mut []);

    assert!(ctrl.index_pulse_active());
    tick_index_flag_sync(&mut ctrl);
    assert!(ctrl.take_index_pulse());
    assert!(ctrl.index_pulse_active());

    ctrl.tick(INDEX_PULSE_CCK - INDEX_FLAG_SYNC_CCK - 1, 0, &mut []);
    assert!(ctrl.index_pulse_active());
    ctrl.tick(1, 0, &mut []);
    assert!(!ctrl.index_pulse_active());

    let _ = fs::remove_file(path);
    Ok(())
}

#[test]
fn motor_off_drive_does_not_emit_index_pulse() -> Result<()> {
    let path = temp_adf()?;
    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: 100,
        drives: [
            Some(FloppyDriveConfig {
                path: path.clone(),
                write_protected: true,
            }),
            None,
            None,
            None,
        ],
    };
    let mut ctrl = FloppyController::from_config(&cfg)?;
    ctrl.write_prb(!CIAB_DSKSEL0);
    ctrl.ensure_track(0, 0);
    ctrl.drives[0].set_rotation_word(encoded_track_words() - 1);
    ctrl.drives[0].rotation_acc_cck = 0;

    assert_eq!(ctrl.next_index_pulse_cck(), None);
    ctrl.tick(ctrl.word_cck(), 0, &mut []);
    assert!(!ctrl.index_pulse_active());
    assert!(!ctrl.take_index_pulse());

    let _ = fs::remove_file(path);
    Ok(())
}

#[test]
fn next_index_pulse_reports_selected_drive_time() -> Result<()> {
    let path = temp_adf()?;
    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: 100,
        drives: [
            Some(FloppyDriveConfig {
                path: path.clone(),
                write_protected: true,
            }),
            None,
            None,
            None,
        ],
    };
    let mut ctrl = FloppyController::from_config(&cfg)?;
    ctrl.write_prb(0xFF);
    assert_eq!(ctrl.next_index_pulse_cck(), None);

    ctrl.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    ctrl.drives[0].set_rotation_word(encoded_track_words() - 2);
    assert_eq!(ctrl.next_index_pulse_cck(), Some(ctrl.word_cck() * 2));

    ctrl.tick(ctrl.word_cck(), 0, &mut []);
    assert_eq!(ctrl.next_index_pulse_cck(), Some(ctrl.word_cck()));

    ctrl.drives[0].rotation_acc_cck = ctrl.word_cck() - 2;
    assert_eq!(ctrl.next_index_pulse_cck(), Some(2));

    let _ = fs::remove_file(path);
    Ok(())
}

#[test]
fn uae_extended_adf_raw_track_exposes_mfm_words() -> Result<()> {
    let raw_words = [0x4489, 0x2AAA, 0x5555, 0xA144];
    let path = temp_ext2_raw(&raw_words)?;
    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: 100,
        drives: [
            Some(FloppyDriveConfig {
                path: path.clone(),
                write_protected: false,
            }),
            None,
            None,
            None,
        ],
    };
    let mut ctrl = FloppyController::from_config(&cfg)?;

    ctrl.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    ctrl.tick(MOTOR_READY_CCK, 0, &mut []);
    ctrl.ensure_track(0, 0);

    assert_eq!(ctrl.drives[0].cached_words(), raw_words);
    assert!(ctrl.drives[0]
        .image
        .as_ref()
        .is_some_and(|image| !image.write_protected));

    let _ = fs::remove_file(path);
    Ok(())
}

#[test]
fn uae_extended_adf_raw_track_preserves_odd_byte_payload() -> Result<()> {
    let path = temp_ext2_track(1, 20, &[0x12, 0x34, 0xA0])?;
    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: 100,
        drives: [
            Some(FloppyDriveConfig {
                path: path.clone(),
                write_protected: false,
            }),
            None,
            None,
            None,
        ],
    };
    let mut ctrl = FloppyController::from_config(&cfg)?;
    let mut chip_ram = vec![0xFF, 0xFF];

    ctrl.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    ctrl.tick(MOTOR_READY_CCK, 0, &mut chip_ram);
    ctrl.ensure_track(0, 0);
    assert_eq!(ctrl.drives[0].cached_words(), [0x1234, 0xA000]);

    ctrl.drives[0].set_rotation_word(0);
    ctrl.drives[0].rotation_acc_cck = 0;
    ctrl.set_dskpt_low(0);
    let len = DSKLEN_DMAEN | DSKLEN_WRITE | 1;
    assert!(!ctrl.write_dsklen(len, 0));
    assert!(!ctrl.write_dsklen(len, 0));
    let dmacon = DMACON_DMAEN | DMACON_DISK;
    let word_cck = FloppyController::word_cck_for_track_words(2);
    assert!(ctrl.tick(word_cck, dmacon, &mut chip_ram));

    let persisted = fs::read(&path)?;
    let desc = &persisted[12..24];
    assert_eq!(
        u32::from_be_bytes([desc[4], desc[5], desc[6], desc[7]]) as usize,
        3
    );
    assert_eq!(
        u32::from_be_bytes([desc[8], desc[9], desc[10], desc[11]]),
        20
    );
    assert_eq!(&persisted[24..27], &[0xFF, 0xFC, 0xA0]);

    let mut reloaded = FloppyController::from_config(&cfg)?;
    reloaded.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    reloaded.tick(MOTOR_READY_CCK, 0, &mut []);
    reloaded.ensure_track(0, 0);
    assert_eq!(reloaded.drives[0].cached_words(), [0xFFFC, 0xA000]);

    let _ = fs::remove_file(path);
    Ok(())
}

#[test]
fn raw_mfm_replacement_preserves_odd_stored_byte_capacity() -> Result<()> {
    let mut words = Vec::new();
    let mut bit_len = 0;
    let mut stored_len = 5;
    let mut revolutions = 3;

    apply_raw_mfm_write(
        &mut words,
        &mut bit_len,
        &mut stored_len,
        &mut revolutions,
        &[0xABCD],
        0,
        0,
        true,
    )?;

    assert_eq!(words, [0xABC8, 0x0000, 0x0000]);
    assert_eq!(bit_len, 16);
    assert_eq!(stored_len, 5);
    assert_eq!(revolutions, 1);
    assert_eq!(
        raw_words_payload(&words, (stored_len * 8) as u32, 0),
        [0xAB, 0xC8, 0x00, 0x00, 0x00]
    );
    Ok(())
}

#[test]
fn uae_extended_adf_raw_track_cycles_stored_revolutions() -> Result<()> {
    let raw_words: [u16; 4] = [0x1111, 0x2222, 0x3333, 0x4444];
    let path = temp_ext2_raw_revolutions(&raw_words, 32, 2)?;
    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: 100,
        drives: [
            Some(FloppyDriveConfig {
                path: path.clone(),
                write_protected: true,
            }),
            None,
            None,
            None,
        ],
    };
    let mut ctrl = FloppyController::from_config(&cfg)?;

    ctrl.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    ctrl.tick(MOTOR_READY_CCK, 0, &mut []);
    clear_index_flag(&mut ctrl);
    ctrl.ensure_track(0, 0);
    ctrl.drives[0].set_rotation_word(0);
    ctrl.drives[0].rotation_acc_cck = 0;

    // Per-revolution: the two captured revolutions are stored separately,
    // each its exact 32-bit length (no concatenation seam).
    assert_eq!(ctrl.drives[0].cached.revs.len(), 2);
    assert_eq!(ctrl.drives[0].cached.revs[0].words, [0x1111, 0x2222]);
    assert_eq!(ctrl.drives[0].cached.revs[1].words, [0x3333, 0x4444]);
    assert_eq!(ctrl.drives[0].cached_index_words(), 2);
    let word_cck = FloppyController::word_cck_for_track_words(2);
    assert_eq!(ctrl.next_index_pulse_cck(), Some(word_cck * 2));
    assert_eq!(ctrl.next_disk_word(0, 0), Some(0x1111));
    assert_eq!(ctrl.next_disk_word(0, 0), Some(0x2222));
    assert!(!ctrl.take_index_pulse());
    tick_index_flag_sync(&mut ctrl);
    assert!(ctrl.take_index_pulse());
    assert_eq!(ctrl.next_disk_word(0, 0), Some(0x3333));
    assert_eq!(ctrl.next_disk_word(0, 0), Some(0x4444));
    assert!(!ctrl.take_index_pulse());
    tick_index_flag_sync(&mut ctrl);
    assert!(ctrl.take_index_pulse());
    assert_eq!(ctrl.next_disk_word(0, 0), Some(0x1111));

    let _ = fs::remove_file(path);
    Ok(())
}

#[test]
fn scp_flux_image_decodes_read_only_raw_mfm_track() -> Result<()> {
    let raw_words = [0x4489, 0x2AAA];
    let path = temp_scp_raw_revolutions(&[&raw_words], 32)?;
    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: 100,
        drives: [
            Some(FloppyDriveConfig {
                path: path.clone(),
                write_protected: false,
            }),
            None,
            None,
            None,
        ],
    };
    let mut ctrl = FloppyController::from_config(&cfg)?;

    ctrl.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    ctrl.tick(MOTOR_READY_CCK, 0, &mut []);
    ctrl.ensure_track(0, 0);

    assert_eq!(ctrl.drives[0].cached_words(), raw_words);
    assert_eq!(ctrl.drives[0].cached_index_words(), raw_words.len());
    assert!(ctrl.drives[0]
        .image
        .as_ref()
        .is_some_and(|image| image.write_protected));

    let _ = fs::remove_file(path);
    Ok(())
}

#[test]
fn scp_flux_decode_resolves_variable_intervals_to_cells() -> Result<()> {
    let path = temp_scp_flux_entries(&[60, 100, 80], 3)?;
    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: 100,
        drives: [
            Some(FloppyDriveConfig {
                path: path.clone(),
                write_protected: true,
            }),
            None,
            None,
            None,
        ],
    };
    let mut ctrl = FloppyController::from_config(&cfg)?;

    ctrl.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    ctrl.tick(MOTOR_READY_CCK, 0, &mut []);
    ctrl.ensure_track(0, 0);

    // The PLL resolves the 1500/2500/2000 ns flux intervals at decode time
    // to three consecutive "1" cells. The recovered bits are stored as a
    // single revolution of exact length; the head then clocks them at a
    // uniform per-revolution rate (the captured flux timing is consumed by
    // the data separator, not retained per bit at runtime).
    assert_eq!(ctrl.drives[0].cached.revs.len(), 1);
    assert_eq!(ctrl.drives[0].cached.revs[0].bit_len, 3);
    assert_eq!(ctrl.drives[0].cached_words(), [0xE000]);

    let _ = fs::remove_file(path);
    Ok(())
}

#[test]
fn scp_flux_image_cycles_stored_revolutions() -> Result<()> {
    let rev0 = [0x4489, 0x2AAA];
    let rev1 = [0x5555, 0xA144];
    let path = temp_scp_raw_revolutions(&[&rev0, &rev1], 32)?;
    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: 100,
        drives: [
            Some(FloppyDriveConfig {
                path: path.clone(),
                write_protected: true,
            }),
            None,
            None,
            None,
        ],
    };
    let mut ctrl = FloppyController::from_config(&cfg)?;

    ctrl.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    ctrl.tick(MOTOR_READY_CCK, 0, &mut []);
    clear_index_flag(&mut ctrl);
    ctrl.ensure_track(0, 0);
    ctrl.drives[0].set_rotation_word(0);
    ctrl.drives[0].rotation_acc_cck = 0;

    // Per-revolution: each captured revolution is stored separately.
    assert_eq!(ctrl.drives[0].cached.revs.len(), 2);
    assert_eq!(ctrl.drives[0].cached.revs[0].words, rev0);
    assert_eq!(ctrl.drives[0].cached.revs[1].words, rev1);
    assert_eq!(ctrl.drives[0].cached_index_words(), 2);
    let word_cck = FloppyController::word_cck_for_track_words(2);
    assert_eq!(ctrl.next_index_pulse_cck(), Some(word_cck * 2));
    assert_eq!(ctrl.next_disk_word(0, 0), Some(rev0[0]));
    assert_eq!(ctrl.next_disk_word(0, 0), Some(rev0[1]));
    assert!(!ctrl.take_index_pulse());
    tick_index_flag_sync(&mut ctrl);
    assert!(ctrl.take_index_pulse());
    assert_eq!(ctrl.next_disk_word(0, 0), Some(rev1[0]));
    assert_eq!(ctrl.next_disk_word(0, 0), Some(rev1[1]));
    assert!(!ctrl.take_index_pulse());
    tick_index_flag_sync(&mut ctrl);
    assert!(ctrl.take_index_pulse());
    assert_eq!(ctrl.next_disk_word(0, 0), Some(rev0[0]));

    let _ = fs::remove_file(path);
    Ok(())
}

#[test]
fn scp_extended_mode_uses_extended_track_table_offset() -> Result<()> {
    let raw_words = [0x4489, 0x2AAA];
    let path = temp_scp_raw_revolutions_with_flags(
        &[&raw_words],
        32,
        SCP_FLAG_INDEX | SCP_FLAG_EXTENDED_MODE,
    )?;
    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: 100,
        drives: [
            Some(FloppyDriveConfig {
                path: path.clone(),
                write_protected: true,
            }),
            None,
            None,
            None,
        ],
    };
    let mut ctrl = FloppyController::from_config(&cfg)?;

    ctrl.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    ctrl.tick(MOTOR_READY_CCK, 0, &mut []);
    ctrl.ensure_track(0, 0);

    assert_eq!(ctrl.drives[0].cached_words(), raw_words);

    let _ = fs::remove_file(path);
    Ok(())
}

#[test]
fn scp_explicit_16_bit_width_keeps_reserved_bytes_as_reserved() -> Result<()> {
    let raw_words = [0x4489, 0x2AAA];
    let path = temp_scp_raw_revolutions(&[&raw_words], 32)?;
    let mut image = fs::read(&path)?;
    image[0x09] = SCP_EXPLICIT_16_BIT_FLUX_WIDTH;
    image[0x0A] = 0x12;
    image[0x0B] = 0x34;
    fs::write(&path, &image)?;

    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: 100,
        drives: [
            Some(FloppyDriveConfig {
                path: path.clone(),
                write_protected: true,
            }),
            None,
            None,
            None,
        ],
    };
    let mut ctrl = FloppyController::from_config(&cfg)?;

    ctrl.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    ctrl.tick(MOTOR_READY_CCK, 0, &mut []);
    ctrl.ensure_track(0, 0);

    assert_eq!(ctrl.drives[0].cached_words(), raw_words);
    assert_eq!(ctrl.drives[0].cached_index_words(), raw_words.len());

    let _ = fs::remove_file(path);
    Ok(())
}

#[test]
fn scp_non_indexed_capture_uses_rpm_for_synthetic_index() -> Result<()> {
    assert_eq!(scp_revolution_bit_len(0, 0)?, Some(100_000));
    assert_eq!(scp_revolution_bit_len(0, SCP_FLAG_RPM_360)?, Some(83_333));

    let raw_words = [0x4489, 0x2AAA];
    let path = temp_scp_raw_revolutions_with_flags(&[&raw_words], 32, 0)?;
    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: 100,
        drives: [
            Some(FloppyDriveConfig {
                path: path.clone(),
                write_protected: true,
            }),
            None,
            None,
            None,
        ],
    };
    let mut ctrl = FloppyController::from_config(&cfg)?;

    ctrl.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    ctrl.tick(MOTOR_READY_CCK, 0, &mut []);
    ctrl.ensure_track(0, 0);

    assert_eq!(&ctrl.drives[0].cached_words()[..raw_words.len()], raw_words);
    assert_eq!(
        ctrl.drives[0].cached_index_words(),
        100_000usize.div_ceil(16)
    );
    assert_eq!(
        ctrl.drives[0].cached_words().len(),
        100_000usize.div_ceil(16)
    );

    let _ = fs::remove_file(path);
    Ok(())
}

#[test]
fn scp_checksum_is_verified_when_present() -> Result<()> {
    let raw_words = [0x4489, 0x2AAA];
    let path = temp_scp_raw_revolutions(&[&raw_words], 32)?;
    let mut image = fs::read(&path)?;
    write_scp_checksum(&mut image);
    fs::write(&path, &image)?;

    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: 100,
        drives: [
            Some(FloppyDriveConfig {
                path: path.clone(),
                write_protected: true,
            }),
            None,
            None,
            None,
        ],
    };
    let mut ctrl = FloppyController::from_config(&cfg)?;
    ctrl.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    ctrl.tick(MOTOR_READY_CCK, 0, &mut []);
    ctrl.ensure_track(0, 0);
    assert_eq!(ctrl.drives[0].cached_words(), raw_words);

    let last = image.len() - 1;
    image[last] ^= 0x01;
    fs::write(&path, &image)?;
    let err = FloppyController::from_config(&cfg)
        .err()
        .expect("corrupt SCP checksum should fail");
    assert!(format!("{err:#}").contains("SCP checksum mismatch"));

    let _ = fs::remove_file(path);
    Ok(())
}

#[test]
fn scp_flux_zero_entries_extend_transition_intervals() -> Result<()> {
    let payload = [0x00, 0x00, 0x00, 0x01];
    let (words, bit_len, bitcell_ns) =
        scp_flux_to_mfm_words(0, 0, &payload, SCP_CAPTURE_BASE_NS, None)?;

    assert_eq!(bit_len, 819);
    assert_eq!(bitcell_ns.len(), bit_len as usize);
    assert_eq!(words.len(), 52);
    assert_eq!(words.last().copied().unwrap() & 0x2000, 0x2000);
    Ok(())
}

#[test]
fn scp_flux_pll_decodes_each_interval_locally_without_drift() -> Result<()> {
    // Two equal 2500 ns intervals (0x64 ticks each). The PLL data separator
    // resolves each interval locally as round(2500/cell) = 1 cell, so the
    // two flux transitions decode to two consecutive "1" cells carrying
    // their measured 2500 ns time. The old cumulative quantizer instead
    // accumulated the 0.25-cell remainder and emitted "101" (3 bits) -- the
    // drift this PLL removes.
    let payload = [0x00, 0x64, 0x00, 0x64];
    let (words, bit_len, bitcell_ns) =
        scp_flux_to_mfm_words(0, 0, &payload, SCP_CAPTURE_BASE_NS, None)?;

    assert_eq!(bit_len, 2);
    assert_eq!(bitcell_ns, [2500, 2500]);
    assert_eq!(words[0] & 0xC000, 0xC000);
    Ok(())
}

#[test]
fn scp_flux_pll_locks_to_offnominal_rate_without_drift() -> Result<()> {
    // A real disk's cell rate is rarely exactly 2 us. Simulate a track
    // spinning slightly fast: 40 flux transitions exactly two cells apart
    // at a 1950 ns cell (3900 ns = 156 SCP ticks per interval). The PLL
    // must lock to 1950 ns and resolve every interval as exactly 2 cells,
    // recovering a clean alternating "01" stream with no accumulated drift.
    // (A fixed-2 us cumulative quantizer drifts ~1 cell every ~13 intervals
    // and would mis-resolve later transitions, corrupting the stream.)
    let payload: Vec<u8> = std::iter::repeat_n([0x00, 0x9C], 40).flatten().collect();
    let (words, bit_len, bitcell_ns) =
        scp_flux_to_mfm_words(0, 0, &payload, SCP_CAPTURE_BASE_NS, None)?;

    assert_eq!(bit_len, 80, "40 two-cell intervals => 80 cells, no drift");
    assert!(
        bitcell_ns.iter().all(|&ns| ns == 1950),
        "PLL should recover a uniform 1950 ns cell"
    );
    // Each interval is one "0" cell then one "1" cell => 0b0101... = 0x5555.
    assert_eq!(words[0], 0x5555);
    assert_eq!(words[1], 0x5555);
    Ok(())
}

#[test]
fn scp_flux_preserves_uneven_bitcell_timing() -> Result<()> {
    let payload = [0x00, 0x3C, 0x00, 0x64, 0x00, 0x50];
    let (words, bit_len, bitcell_ns) =
        scp_flux_to_mfm_words(0, 0, &payload, SCP_CAPTURE_BASE_NS, None)?;

    assert_eq!(bit_len, 3);
    assert_eq!(bitcell_ns, [1500, 2500, 2000]);
    assert_eq!(words[0] & 0xE000, 0xE000);
    Ok(())
}

#[test]
fn extended_track_length_controls_index_timing() -> Result<()> {
    let raw_words = [0x1111, 0x2222, 0x3333, 0x4444];
    let path = temp_ext2_raw(&raw_words)?;
    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: 100,
        drives: [
            Some(FloppyDriveConfig {
                path: path.clone(),
                write_protected: true,
            }),
            None,
            None,
            None,
        ],
    };
    let mut ctrl = FloppyController::from_config(&cfg)?;

    ctrl.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    ctrl.tick(MOTOR_READY_CCK, 0, &mut []);
    clear_index_flag(&mut ctrl);
    ctrl.ensure_track(0, 0);
    ctrl.drives[0].set_rotation_word(raw_words.len() - 1);
    ctrl.drives[0].rotation_acc_cck = 0;

    let word_cck = FloppyController::word_cck_for_track_words(raw_words.len());
    let remaining_cck = word_cck
        .saturating_sub(ctrl.drives[0].rotation_acc_cck)
        .max(1);
    assert_eq!(ctrl.next_index_pulse_cck(), Some(remaining_cck));

    ctrl.tick(word_cck, 0, &mut []);
    assert!(!ctrl.take_index_pulse());
    tick_index_flag_sync(&mut ctrl);
    assert!(ctrl.take_index_pulse());

    let _ = fs::remove_file(path);
    Ok(())
}

#[test]
fn one_word_raw_tracks_do_not_advertise_index_deadlines() -> Result<()> {
    let raw_words = [0x4489];
    let path = temp_ext2_raw(&raw_words)?;
    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: 100,
        drives: [
            Some(FloppyDriveConfig {
                path: path.clone(),
                write_protected: true,
            }),
            None,
            None,
            None,
        ],
    };
    let mut ctrl = FloppyController::from_config(&cfg)?;

    ctrl.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    ctrl.tick(MOTOR_READY_CCK, 0, &mut []);
    clear_index_flag(&mut ctrl);
    ctrl.ensure_track(0, 0);

    assert_eq!(ctrl.next_index_pulse_cck(), None);
    ctrl.tick(FloppyController::word_cck_for_track_words(1), 0, &mut []);
    assert!(!ctrl.index_pulse_active());
    assert!(!ctrl.take_index_pulse());

    let _ = fs::remove_file(path);
    Ok(())
}

#[test]
fn raw_track_write_dma_uses_raw_index_timing() -> Result<()> {
    let raw_words = [0x1111, 0x2222, 0x3333, 0x4444];
    let path = temp_ext2_raw(&raw_words)?;
    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: 100,
        drives: [
            Some(FloppyDriveConfig {
                path: path.clone(),
                write_protected: false,
            }),
            None,
            None,
            None,
        ],
    };
    let mut ctrl = FloppyController::from_config(&cfg)?;
    let mut chip_ram = vec![0xAB, 0xCD];

    ctrl.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    ctrl.tick(MOTOR_READY_CCK, 0, &mut chip_ram);
    clear_index_flag(&mut ctrl);
    ctrl.ensure_track(0, 0);
    ctrl.drives[0].set_rotation_word(raw_words.len() - 1);
    ctrl.drives[0].rotation_acc_cck = 0;
    ctrl.set_dskpt_low(0);
    let len = DSKLEN_DMAEN | DSKLEN_WRITE | 1;
    assert!(!ctrl.write_dsklen(len, 0));
    assert!(!ctrl.write_dsklen(len, 0));

    let dmacon = DMACON_DMAEN | DMACON_DISK;
    let word_cck = FloppyController::word_cck_for_track_words(raw_words.len());
    assert!(ctrl.tick(word_cck, dmacon, &mut chip_ram));
    assert!(!ctrl.take_index_pulse());
    tick_index_flag_sync(&mut ctrl);
    assert!(ctrl.take_index_pulse());
    assert_eq!(ctrl.drives[0].rotation_word_index(), 0);

    let _ = fs::remove_file(path);
    Ok(())
}

#[test]
fn write_dma_armed_against_empty_bay_lands_at_post_insert_rotation() -> Result<()> {
    let raw_words = [0x1111, 0x2222, 0x3333, 0x4444];
    let path = temp_ext2_raw(&raw_words)?;
    let mut ctrl = FloppyController::default();
    let mut chip_ram = vec![0xAB, 0xCD];
    let dmacon = DMACON_DMAEN | DMACON_DISK;

    // DF0 selected, motor line on, empty bay -- with the mechanism's last
    // rotation having left the head mid-track. The write arms and pends;
    // the rotational start it latches now is from a platter phase that
    // will not survive the media insert.
    ctrl.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    ctrl.tick(MOTOR_READY_CCK, 0, &mut []);
    ctrl.drives[0].set_rotation_word(2);
    ctrl.drives[0].rotation_acc_cck = 0;
    ctrl.set_dskpt_low(0);
    let len = DSKLEN_DMAEN | DSKLEN_WRITE | 1;
    assert!(!ctrl.write_dsklen(len, 0));
    assert!(!ctrl.write_dsklen(len, 0));

    // Nothing moves against the empty bay.
    let word_cck = FloppyController::word_cck_for_track_words(raw_words.len());
    assert!(!ctrl.tick(word_cck * 125, dmacon, &mut chip_ram));

    // A disk arrives: rotation restarts at the index, and the pending
    // write lands where the head really is when cells first go down --
    // word 0 -- not at the stale position latched against the empty bay.
    ctrl.insert_disk_image(0, path.clone(), false)?;
    assert!(ctrl.tick(word_cck, dmacon, &mut chip_ram));

    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: 100,
        drives: [
            Some(FloppyDriveConfig {
                path: path.clone(),
                write_protected: false,
            }),
            None,
            None,
            None,
        ],
    };
    let mut reloaded = FloppyController::from_config(&cfg)?;
    reloaded.ensure_track(0, 0);
    assert_eq!(
        reloaded.drives[0].cached_words(),
        [0xABC9, 0x2222, 0x3333, 0x4444]
    );

    let _ = fs::remove_file(path);
    Ok(())
}

#[test]
fn speculative_write_holds_the_host_file_until_commit() -> Result<()> {
    let raw_words = [0x1111, 0x2222, 0x3333, 0x4444];
    let path = temp_ext2_raw(&raw_words)?;
    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: 100,
        drives: [
            Some(FloppyDriveConfig {
                path: path.clone(),
                write_protected: false,
            }),
            None,
            None,
            None,
        ],
    };
    let mut ctrl = FloppyController::from_config(&cfg)?;
    let mut chip_ram = vec![0xAB, 0xCD];
    let dmacon = DMACON_DMAEN | DMACON_DISK;

    ctrl.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    ctrl.tick(MOTOR_READY_CCK, 0, &mut chip_ram);
    ctrl.ensure_track(0, 0);
    ctrl.drives[0].set_rotation_word(0);
    ctrl.drives[0].rotation_acc_cck = 0;
    ctrl.set_dskpt_low(0);

    // The write completes inside a speculative run-ahead frame: the
    // in-memory image takes it (the anchor restore would rewind it), the
    // host file must not.
    ctrl.set_speculative_writes(true);
    let len = DSKLEN_DMAEN | DSKLEN_WRITE | 1;
    assert!(!ctrl.write_dsklen(len, 0));
    assert!(!ctrl.write_dsklen(len, 0));
    let word_cck = FloppyController::word_cck_for_track_words(raw_words.len());
    assert!(ctrl.tick(word_cck, dmacon, &mut chip_ram));
    ctrl.set_speculative_writes(false);

    ctrl.ensure_track(0, 0);
    assert_eq!(
        ctrl.drives[0].cached_words(),
        [0xABC9, 0x2222, 0x3333, 0x4444],
        "the in-memory image carries the write"
    );
    {
        let mut untouched = FloppyController::from_config(&cfg)?;
        untouched.ensure_track(0, 0);
        assert_eq!(
            untouched.drives[0].cached_words(),
            raw_words,
            "the host file does not"
        );
    }

    // An early burst break commits the frames that wrote: the held
    // write-through is delivered.
    ctrl.commit_speculative_writes();
    let mut reloaded = FloppyController::from_config(&cfg)?;
    reloaded.ensure_track(0, 0);
    assert_eq!(
        reloaded.drives[0].cached_words(),
        [0xABC9, 0x2222, 0x3333, 0x4444]
    );

    let _ = fs::remove_file(path);
    Ok(())
}

#[test]
fn cpu_dskdat_write_without_dma_overlays_raw_track() -> Result<()> {
    let raw_words = [0x1111, 0x2222, 0x3333, 0x4444];
    let path = temp_ext2_raw(&raw_words)?;
    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: 100,
        drives: [
            Some(FloppyDriveConfig {
                path: path.clone(),
                write_protected: false,
            }),
            None,
            None,
            None,
        ],
    };
    let mut ctrl = FloppyController::from_config(&cfg)?;

    ctrl.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    ctrl.tick(MOTOR_READY_CCK, 0, &mut []);
    ctrl.ensure_track(0, 0);
    ctrl.drives[0].set_rotation_word(1);
    ctrl.drives[0].rotation_acc_cck = 0;

    assert!(!ctrl.write_dsklen(DSKLEN_WRITE | 1, 0));
    let status = ctrl.read_dskbytr(0, 0);
    assert_eq!(status & DMAON, 0);
    assert_ne!(status & DISKWRITE, 0);

    ctrl.write_dskdat(0xABCD);

    let mut reloaded = FloppyController::from_config(&cfg)?;
    reloaded.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    reloaded.tick(MOTOR_READY_CCK, 0, &mut []);
    reloaded.ensure_track(0, 0);
    assert_eq!(
        reloaded.drives[0].cached_words(),
        [0x1111, 0xABCA, 0x3333, 0x4444]
    );

    let _ = fs::remove_file(path);
    Ok(())
}

#[test]
fn uae_extended_adf_amigados_track_encodes_sectors() -> Result<()> {
    let mut track_data = vec![0u8; SECTORS_PER_TRACK * BYTES_PER_SECTOR];
    track_data[0..BYTES_PER_SECTOR].fill(0x5A);
    let path = temp_ext2_amigados(&track_data)?;
    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: 100,
        drives: [
            Some(FloppyDriveConfig {
                path: path.clone(),
                write_protected: true,
            }),
            None,
            None,
            None,
        ],
    };
    let mut ctrl = FloppyController::from_config(&cfg)?;

    ctrl.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    ctrl.tick(MOTOR_READY_CCK, 0, &mut []);
    ctrl.ensure_track(0, 0);
    let decoded = decode_track_write(0, &ctrl.drives[0].cached_words())?;

    let sector0 = decoded.iter().find(|(sector, _)| *sector == 0).unwrap();
    assert_eq!(&sector0.1[..], &[0x5A; BYTES_PER_SECTOR]);
    assert_eq!(decoded.len(), SECTORS_PER_TRACK);

    let _ = fs::remove_file(path);
    Ok(())
}

#[test]
fn uae_extended_adf_amigados_track_uses_bit_length_before_zero_padding() -> Result<()> {
    let mut track_data = vec![0u8; SECTORS_PER_TRACK * BYTES_PER_SECTOR];
    track_data[0..BYTES_PER_SECTOR].fill(0x5A);
    let mut payload = track_data.clone();
    payload.resize(0x31f0, 0);
    let path = temp_ext2_track(0, (track_data.len() * 8) as u32, &payload)?;
    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: 100,
        drives: [
            Some(FloppyDriveConfig {
                path: path.clone(),
                write_protected: true,
            }),
            None,
            None,
            None,
        ],
    };
    let mut ctrl = FloppyController::from_config(&cfg)?;

    ctrl.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    ctrl.tick(MOTOR_READY_CCK, 0, &mut []);
    ctrl.ensure_track(0, 0);
    let decoded = decode_track_write(0, &ctrl.drives[0].cached_words())?;

    let sector0 = decoded.iter().find(|(sector, _)| *sector == 0).unwrap();
    assert_eq!(&sector0.1[..], &[0x5A; BYTES_PER_SECTOR]);
    assert_eq!(decoded.len(), SECTORS_PER_TRACK);

    let _ = fs::remove_file(path);
    Ok(())
}

#[test]
fn uae_extended_adf_amigados_track_rejects_nonzero_padding_after_bit_length() {
    let track_data = vec![0u8; SECTORS_PER_TRACK * BYTES_PER_SECTOR];
    let mut payload = track_data.clone();
    payload.resize(0x31f0, 0);
    *payload.last_mut().unwrap() = 0x5A;
    let image = ext2_track_image(0, (track_data.len() * 8) as u32, 1, &payload);

    let err = match decode_uae_extended_adf(&image) {
        Ok(_) => panic!("non-zero padding after AmigaDOS bit length should fail"),
        Err(err) => err,
    };
    assert!(err
        .to_string()
        .contains("AmigaDOS padding after bit length is non-zero"));
}

#[test]
fn uae_extended_adf_blank_amigados_track_without_sector_payload_is_empty() -> Result<()> {
    let payload = vec![0u8; 0x31f0];
    let image = ext2_track_image(0, (payload.len() * 8) as u32, 1, &payload);

    let FloppyImageData::Tracks(tracks) = decode_uae_extended_adf(&image)? else {
        panic!("UAE extended ADF should decode to per-track data");
    };
    assert!(tracks[0].is_none());
    Ok(())
}

#[test]
fn writable_extended_adf_amigados_track_persists_sector_updates() -> Result<()> {
    let path = temp_ext2_amigados(&vec![0u8; SECTORS_PER_TRACK * BYTES_PER_SECTOR])?;
    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: 100,
        drives: [
            Some(FloppyDriveConfig {
                path: path.clone(),
                write_protected: false,
            }),
            None,
            None,
            None,
        ],
    };
    let mut ctrl = FloppyController::from_config(&cfg)?;
    let mut source = vec![0u8; SECTORS_PER_TRACK * BYTES_PER_SECTOR];
    source[0..BYTES_PER_SECTOR].fill(0xA5);
    let words = encode_amigados_track(0, &source);
    let mut chip_ram = vec![0u8; words.len() * 2 + 2];
    for (i, word) in words.iter().copied().enumerate() {
        let [hi, lo] = word.to_be_bytes();
        chip_ram[i * 2] = hi;
        chip_ram[i * 2 + 1] = lo;
    }

    ctrl.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    ctrl.tick(MOTOR_READY_CCK, 0, &mut chip_ram);
    ctrl.set_dskpt_low(0);
    let len = DSKLEN_DMAEN | DSKLEN_WRITE | (words.len() as u16 & DSKLEN_MASK);
    assert!(!ctrl.write_dsklen(len, 0));
    assert!(!ctrl.write_dsklen(len, 0));
    let dmacon = DMACON_DMAEN | DMACON_DISK;
    while !ctrl.tick(ctrl.word_cck(), dmacon, &mut chip_ram) {}

    let persisted = fs::read(&path)?;
    assert_eq!(&persisted[0..8], UAE_EXT2_SIGNATURE);
    let payload_off = 8 + 4 + 12;
    assert_eq!(
        &persisted[payload_off..payload_off + BYTES_PER_SECTOR],
        &[0xA5; BYTES_PER_SECTOR]
    );

    let mut reloaded = FloppyController::from_config(&cfg)?;
    reloaded.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    reloaded.tick(MOTOR_READY_CCK, 0, &mut []);
    reloaded.ensure_track(0, 0);
    let decoded = decode_track_write(0, &reloaded.drives[0].cached_words())?;
    let sector0 = decoded.iter().find(|(sector, _)| *sector == 0).unwrap();
    assert_eq!(&sector0.1[..], &[0xA5; BYTES_PER_SECTOR]);

    let _ = fs::remove_file(path);
    Ok(())
}

#[test]
fn writable_extended_adf_preserves_multi_revolution_raw_track_payload() -> Result<()> {
    let raw_words: [u16; 4] = [0x1111, 0x2222, 0x3333, 0x4444];
    let raw_payload: Vec<u8> = raw_words
        .iter()
        .copied()
        .flat_map(u16::to_be_bytes)
        .collect();
    let path = temp_ext2_amigados_plus_raw(
        &vec![0u8; SECTORS_PER_TRACK * BYTES_PER_SECTOR],
        &raw_words,
        32,
        2,
    )?;
    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: 100,
        drives: [
            Some(FloppyDriveConfig {
                path: path.clone(),
                write_protected: false,
            }),
            None,
            None,
            None,
        ],
    };
    let mut ctrl = FloppyController::from_config(&cfg)?;
    let mut source = vec![0u8; SECTORS_PER_TRACK * BYTES_PER_SECTOR];
    source[0..BYTES_PER_SECTOR].fill(0xA5);
    let words = encode_amigados_track(0, &source);
    let mut chip_ram = vec![0u8; words.len() * 2 + 2];
    for (i, word) in words.iter().copied().enumerate() {
        let [hi, lo] = word.to_be_bytes();
        chip_ram[i * 2] = hi;
        chip_ram[i * 2 + 1] = lo;
    }

    ctrl.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    ctrl.tick(MOTOR_READY_CCK, 0, &mut chip_ram);
    ctrl.set_dskpt_low(0);
    let len = DSKLEN_DMAEN | DSKLEN_WRITE | (words.len() as u16 & DSKLEN_MASK);
    assert!(!ctrl.write_dsklen(len, 0));
    assert!(!ctrl.write_dsklen(len, 0));
    let dmacon = DMACON_DMAEN | DMACON_DISK;
    while !ctrl.tick(ctrl.word_cck(), dmacon, &mut chip_ram) {}

    let persisted = fs::read(&path)?;
    let track0_len = SECTORS_PER_TRACK * BYTES_PER_SECTOR;
    let raw_desc = &persisted[24..36];
    assert_eq!(raw_desc[2], 1);
    assert_eq!(raw_desc[3], 1);
    assert_eq!(
        u32::from_be_bytes([raw_desc[4], raw_desc[5], raw_desc[6], raw_desc[7]]) as usize,
        raw_payload.len()
    );
    assert_eq!(
        u32::from_be_bytes([raw_desc[8], raw_desc[9], raw_desc[10], raw_desc[11]]),
        32
    );
    let raw_payload_off = 12 + 2 * 12 + track0_len;
    assert_eq!(
        &persisted[raw_payload_off..raw_payload_off + raw_payload.len()],
        &raw_payload[..]
    );

    let _ = fs::remove_file(path);
    Ok(())
}

#[test]
fn writable_extended_adf_raw_track_overlays_word_stream() -> Result<()> {
    let raw_words = [0x4489, 0x2AAA, 0x5555, 0xA144];
    let path = temp_ext2_raw(&raw_words)?;
    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: 100,
        drives: [
            Some(FloppyDriveConfig {
                path: path.clone(),
                write_protected: false,
            }),
            None,
            None,
            None,
        ],
    };
    let mut ctrl = FloppyController::from_config(&cfg)?;
    let words: [u16; 2] = [0xDEAD, 0xBEEF];
    let mut chip_ram = vec![0u8; words.len() * 2 + 2];
    for (i, word) in words.iter().copied().enumerate() {
        let [hi, lo] = word.to_be_bytes();
        chip_ram[i * 2] = hi;
        chip_ram[i * 2 + 1] = lo;
    }

    ctrl.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    ctrl.tick(MOTOR_READY_CCK, 0, &mut chip_ram);
    ctrl.ensure_track(0, 0);
    ctrl.drives[0].set_rotation_word(2);
    ctrl.drives[0].rotation_acc_cck = 0;
    ctrl.set_dskpt_low(0);
    let len = DSKLEN_DMAEN | DSKLEN_WRITE | words.len() as u16;
    assert!(!ctrl.write_dsklen(len, 0));
    assert!(!ctrl.write_dsklen(len, 0));
    let dmacon = DMACON_DMAEN | DMACON_DISK;
    while !ctrl.tick(ctrl.word_cck(), dmacon, &mut chip_ram) {}

    let persisted = fs::read(&path)?;
    let desc = &persisted[12..24];
    assert_eq!(desc[2], 0);
    assert_eq!(desc[3], 1);
    assert_eq!(
        u32::from_be_bytes([desc[4], desc[5], desc[6], desc[7]]) as usize,
        raw_words.len() * 2
    );
    assert_eq!(
        u32::from_be_bytes([desc[8], desc[9], desc[10], desc[11]]),
        64
    );
    assert_eq!(
        &persisted[24..32],
        &[0x44, 0x89, 0x2A, 0xAA, 0xDE, 0xAD, 0xBE, 0xEC]
    );

    let mut reloaded = FloppyController::from_config(&cfg)?;
    reloaded.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    reloaded.tick(MOTOR_READY_CCK, 0, &mut []);
    reloaded.ensure_track(0, 0);
    assert_eq!(
        reloaded.drives[0].cached_words(),
        [0x4489, 0x2AAA, 0xDEAD, 0xBEEC]
    );

    let _ = fs::remove_file(path);
    Ok(())
}

#[test]
fn writable_extended_adf_raw_track_overlays_bit_phase() -> Result<()> {
    let raw_words = [0x0000, 0x0000, 0x0000, 0x0000];
    let path = temp_ext2_raw(&raw_words)?;
    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: 100,
        drives: [
            Some(FloppyDriveConfig {
                path: path.clone(),
                write_protected: false,
            }),
            None,
            None,
            None,
        ],
    };
    let mut ctrl = FloppyController::from_config(&cfg)?;
    let mut chip_ram = vec![0u8; 64];
    chip_ram[0..2].copy_from_slice(&0xFFFFu16.to_be_bytes());

    ctrl.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    ctrl.tick(MOTOR_READY_CCK, 0, &mut chip_ram);
    ctrl.ensure_track(0, 0);
    // Head at word 1, 8 cells in (bit 24) -- the write's landing bit phase.
    ctrl.drives[0].set_rotation_bit(24);
    let word_cck = FloppyController::word_cck_for_track_words(raw_words.len());
    ctrl.set_dskpt_low(0);
    let len = DSKLEN_DMAEN | DSKLEN_WRITE | 1;
    assert!(!ctrl.write_dsklen(len, 0));
    assert!(!ctrl.write_dsklen(len, 0));
    let dmacon = DMACON_DMAEN | DMACON_DISK;
    while !ctrl.tick(word_cck, dmacon, &mut chip_ram) {}

    let mut reloaded = FloppyController::from_config(&cfg)?;
    reloaded.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    reloaded.tick(MOTOR_READY_CCK, 0, &mut []);
    reloaded.ensure_track(0, 0);
    assert_eq!(
        reloaded.drives[0].cached_words(),
        [0x0000, 0x00FF, 0xF800, 0x0000]
    );

    let _ = fs::remove_file(path);
    Ok(())
}

#[test]
fn writable_extended_adf_raw_track_wraps_at_partial_bit_len() -> Result<()> {
    let raw_words = [0x0000, 0x0000];
    let path = temp_ext2_raw_revolutions(&raw_words, 20, 1)?;
    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: 100,
        drives: [
            Some(FloppyDriveConfig {
                path: path.clone(),
                write_protected: false,
            }),
            None,
            None,
            None,
        ],
    };
    let mut ctrl = FloppyController::from_config(&cfg)?;
    let mut chip_ram = vec![0u8; 64];
    chip_ram[0..2].copy_from_slice(&0xFFFFu16.to_be_bytes());

    ctrl.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    ctrl.tick(MOTOR_READY_CCK, 0, &mut chip_ram);
    ctrl.ensure_track(0, 0);
    // Head at word 1, 4 cells in (bit 20 == bit_len, wraps to the start).
    ctrl.drives[0].set_rotation_bit(20);
    let word_cck = FloppyController::word_cck_for_track_words(raw_words.len());
    ctrl.set_dskpt_low(0);
    let len = DSKLEN_DMAEN | DSKLEN_WRITE | 1;
    assert!(!ctrl.write_dsklen(len, 0));
    assert!(!ctrl.write_dsklen(len, 0));
    let dmacon = DMACON_DMAEN | DMACON_DISK;
    while !ctrl.tick(word_cck, dmacon, &mut chip_ram) {}

    let persisted = fs::read(&path)?;
    let desc = &persisted[12..24];
    assert_eq!(
        u32::from_be_bytes([desc[8], desc[9], desc[10], desc[11]]),
        20
    );
    assert_eq!(&persisted[24..28], &[0xFF, 0xF8, 0x00, 0x00]);

    let mut reloaded = FloppyController::from_config(&cfg)?;
    reloaded.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    reloaded.tick(MOTOR_READY_CCK, 0, &mut []);
    reloaded.ensure_track(0, 0);
    assert_eq!(reloaded.drives[0].cached_words(), [0xFFF8, 0x0000]);

    let _ = fs::remove_file(path);
    Ok(())
}

#[test]
fn writable_extended_adf_raw_track_preserves_partial_word_tail() -> Result<()> {
    let path = temp_ext2_track(1, 20, &[0xFF, 0xFF, 0xF0])?;
    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: 100,
        drives: [
            Some(FloppyDriveConfig {
                path: path.clone(),
                write_protected: false,
            }),
            None,
            None,
            None,
        ],
    };
    let mut ctrl = FloppyController::from_config(&cfg)?;
    let mut chip_ram = vec![0u8; 2];

    ctrl.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    ctrl.tick(MOTOR_READY_CCK, 0, &mut chip_ram);
    ctrl.ensure_track(0, 0);
    assert_eq!(ctrl.drives[0].cached_words(), [0xFFFF, 0xF000]);
    ctrl.drives[0].set_rotation_word(0);
    ctrl.drives[0].rotation_acc_cck = 0;
    ctrl.set_dskpt_low(0);
    let len = DSKLEN_DMAEN | DSKLEN_WRITE | 1;
    assert!(!ctrl.write_dsklen(len, 0));
    assert!(!ctrl.write_dsklen(len, 0));
    let dmacon = DMACON_DMAEN | DMACON_DISK;
    while !ctrl.tick(ctrl.word_cck(), dmacon, &mut chip_ram) {}

    let persisted = fs::read(&path)?;
    let desc = &persisted[12..24];
    assert_eq!(
        u32::from_be_bytes([desc[4], desc[5], desc[6], desc[7]]) as usize,
        3
    );
    assert_eq!(
        u32::from_be_bytes([desc[8], desc[9], desc[10], desc[11]]),
        20
    );
    assert_eq!(&persisted[24..27], &[0x00, 0x07, 0xF0]);

    let mut reloaded = FloppyController::from_config(&cfg)?;
    reloaded.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    reloaded.tick(MOTOR_READY_CCK, 0, &mut []);
    reloaded.ensure_track(0, 0);
    assert_eq!(reloaded.drives[0].cached_words(), [0x0007, 0xF000]);

    let _ = fs::remove_file(path);
    Ok(())
}

#[test]
fn raw_track_write_dma_loses_last_three_output_bits() -> Result<()> {
    let raw_words = [0xFFFF];
    let path = temp_ext2_raw(&raw_words)?;
    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: 100,
        drives: [
            Some(FloppyDriveConfig {
                path: path.clone(),
                write_protected: false,
            }),
            None,
            None,
            None,
        ],
    };
    let mut ctrl = FloppyController::from_config(&cfg)?;
    let mut chip_ram = vec![0x00, 0x00];

    ctrl.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    ctrl.tick(MOTOR_READY_CCK, 0, &mut chip_ram);
    ctrl.ensure_track(0, 0);
    ctrl.drives[0].set_rotation_word(0);
    ctrl.drives[0].rotation_acc_cck = 0;
    ctrl.set_dskpt_low(0);
    let len = DSKLEN_DMAEN | DSKLEN_WRITE | 1;
    assert!(!ctrl.write_dsklen(len, 0));
    assert!(!ctrl.write_dsklen(len, 0));
    let dmacon = DMACON_DMAEN | DMACON_DISK;
    assert!(ctrl.tick(
        FloppyController::word_cck_for_track_words(raw_words.len()),
        dmacon,
        &mut chip_ram
    ));

    let mut reloaded = FloppyController::from_config(&cfg)?;
    reloaded.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    reloaded.tick(MOTOR_READY_CCK, 0, &mut []);
    reloaded.ensure_track(0, 0);
    assert_eq!(reloaded.drives[0].cached_words(), [0x0007]);

    let _ = fs::remove_file(path);
    Ok(())
}

#[test]
fn protected_raw_track_write_dma_leaves_media_unchanged() -> Result<()> {
    let raw_words = [0x1111, 0x2222];
    let path = temp_ext2_raw(&raw_words)?;
    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: 100,
        drives: [
            Some(FloppyDriveConfig {
                path: path.clone(),
                write_protected: true,
            }),
            None,
            None,
            None,
        ],
    };
    let mut ctrl = FloppyController::from_config(&cfg)?;
    let mut chip_ram = vec![0xAA, 0xAA];

    ctrl.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    ctrl.tick(MOTOR_READY_CCK, 0, &mut chip_ram);
    ctrl.ensure_track(0, 0);
    ctrl.drives[0].set_rotation_word(0);
    ctrl.drives[0].rotation_acc_cck = 0;
    ctrl.set_dskpt_low(0);
    let len = DSKLEN_DMAEN | DSKLEN_WRITE | 1;
    assert!(!ctrl.write_dsklen(len, 0));
    assert!(!ctrl.write_dsklen(len, 0));
    let dmacon = DMACON_DMAEN | DMACON_DISK;
    assert!(ctrl.tick(
        FloppyController::word_cck_for_track_words(raw_words.len()),
        dmacon,
        &mut chip_ram
    ));

    let mut reloaded = FloppyController::from_config(&cfg)?;
    reloaded.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    reloaded.tick(MOTOR_READY_CCK, 0, &mut []);
    reloaded.ensure_track(0, 0);
    assert_eq!(reloaded.drives[0].cached_words(), raw_words);

    let _ = fs::remove_file(path);
    Ok(())
}

#[test]
fn raw_write_dma_abort_persists_captured_words() -> Result<()> {
    let raw_words = [0x1111, 0x2222, 0x3333, 0x4444];
    let path = temp_ext2_raw(&raw_words)?;
    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: 100,
        drives: [
            Some(FloppyDriveConfig {
                path: path.clone(),
                write_protected: false,
            }),
            None,
            None,
            None,
        ],
    };
    let mut ctrl = FloppyController::from_config(&cfg)?;
    let mut chip_ram = vec![0u8; 64];
    chip_ram[0..2].copy_from_slice(&0xAAAAu16.to_be_bytes());

    ctrl.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    ctrl.tick(MOTOR_READY_CCK, 0, &mut chip_ram);
    ctrl.ensure_track(0, 0);
    ctrl.drives[0].set_rotation_word(1);
    ctrl.drives[0].rotation_acc_cck = 0;

    let len = DSKLEN_DMAEN | DSKLEN_WRITE | 3;
    assert!(!ctrl.write_dsklen(len, 0));
    assert!(!ctrl.write_dsklen(len, 0));
    let dmacon = DMACON_DMAEN | DMACON_DISK;
    let word_cck = FloppyController::word_cck_for_track_words(raw_words.len());
    assert!(!ctrl.tick(word_cck, dmacon, &mut chip_ram));
    assert!(!ctrl.write_dsklen(0, 0));

    let mut reloaded = FloppyController::from_config(&cfg)?;
    reloaded.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    reloaded.tick(MOTOR_READY_CCK, 0, &mut []);
    reloaded.ensure_track(0, 0);
    assert_eq!(
        reloaded.drives[0].cached_words(),
        [0x1111, 0xAAAA, 0x3333, 0x4444]
    );

    let _ = fs::remove_file(path);
    Ok(())
}

#[test]
fn active_write_dma_dsklen_rewrite_updates_remaining_length() -> Result<()> {
    let raw_words = [0x1111, 0x2222, 0x3333, 0x4444];
    let path = temp_ext2_raw(&raw_words)?;
    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: 100,
        drives: [
            Some(FloppyDriveConfig {
                path: path.clone(),
                write_protected: false,
            }),
            None,
            None,
            None,
        ],
    };
    let mut ctrl = FloppyController::from_config(&cfg)?;
    let words = [0xAAAAu16, 0xBBBB, 0xCCCC];
    let mut chip_ram = vec![0u8; words.len() * 2];
    for (i, word) in words.iter().copied().enumerate() {
        let [hi, lo] = word.to_be_bytes();
        chip_ram[i * 2] = hi;
        chip_ram[i * 2 + 1] = lo;
    }

    ctrl.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    ctrl.tick(MOTOR_READY_CCK, 0, &mut chip_ram);
    ctrl.ensure_track(0, 0);
    ctrl.drives[0].set_rotation_word(1);
    ctrl.drives[0].rotation_acc_cck = 0;
    ctrl.set_dskpt_low(0);

    let len = DSKLEN_DMAEN | DSKLEN_WRITE | 3;
    assert!(!ctrl.write_dsklen(len, 0));
    assert!(!ctrl.write_dsklen(len, 0));
    let dmacon = DMACON_DMAEN | DMACON_DISK;
    let word_cck = FloppyController::word_cck_for_track_words(raw_words.len());
    assert!(!ctrl.tick(word_cck, dmacon, &mut chip_ram));

    let shorter = DSKLEN_DMAEN | DSKLEN_WRITE | 1;
    assert!(!ctrl.write_dsklen(shorter, 0));
    assert!(!ctrl.write_dsklen(shorter, 0));
    assert!(ctrl.tick(word_cck, dmacon, &mut chip_ram));

    let mut reloaded = FloppyController::from_config(&cfg)?;
    reloaded.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    reloaded.tick(MOTOR_READY_CCK, 0, &mut []);
    reloaded.ensure_track(0, 0);
    assert_eq!(
        reloaded.drives[0].cached_words(),
        [0x1111, 0xAAAA, 0xBBBB, 0x4444]
    );

    let _ = fs::remove_file(path);
    Ok(())
}

#[test]
fn active_write_dma_dsklen_zero_rewrite_finishes_now() -> Result<()> {
    let raw_words = [0x1111, 0x2222, 0x3333, 0x4444];
    let path = temp_ext2_raw(&raw_words)?;
    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: 100,
        drives: [
            Some(FloppyDriveConfig {
                path: path.clone(),
                write_protected: false,
            }),
            None,
            None,
            None,
        ],
    };
    let mut ctrl = FloppyController::from_config(&cfg)?;
    let mut chip_ram = vec![0u8; 4];
    chip_ram[0..2].copy_from_slice(&0xAAAAu16.to_be_bytes());
    chip_ram[2..4].copy_from_slice(&0xBBBBu16.to_be_bytes());

    ctrl.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    ctrl.tick(MOTOR_READY_CCK, 0, &mut chip_ram);
    ctrl.ensure_track(0, 0);
    ctrl.drives[0].set_rotation_word(1);
    ctrl.drives[0].rotation_acc_cck = 0;
    ctrl.set_dskpt_low(0);

    let len = DSKLEN_DMAEN | DSKLEN_WRITE | 2;
    assert!(!ctrl.write_dsklen(len, 0));
    assert!(!ctrl.write_dsklen(len, 0));
    let dmacon = DMACON_DMAEN | DMACON_DISK;
    let word_cck = FloppyController::word_cck_for_track_words(raw_words.len());
    assert!(!ctrl.tick(word_cck, dmacon, &mut chip_ram));

    assert!(ctrl.write_dsklen(DSKLEN_DMAEN | DSKLEN_WRITE, 0));
    assert!(!ctrl.tick(word_cck, dmacon, &mut chip_ram));

    let mut reloaded = FloppyController::from_config(&cfg)?;
    reloaded.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    reloaded.tick(MOTOR_READY_CCK, 0, &mut []);
    reloaded.ensure_track(0, 0);
    assert_eq!(
        reloaded.drives[0].cached_words(),
        [0x1111, 0xAAAA, 0x3333, 0x4444]
    );

    let _ = fs::remove_file(path);
    Ok(())
}

#[test]
fn active_write_dma_splits_output_when_head_steps_to_new_track() -> Result<()> {
    let track0 = [0x0000, 0x0000];
    let track2 = [0xFFFF, 0xFFFF];
    let path = temp_ext2_raw_tracks(&[(0, &track0), (2, &track2)])?;
    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: 100,
        drives: [
            Some(FloppyDriveConfig {
                path: path.clone(),
                write_protected: false,
            }),
            None,
            None,
            None,
        ],
    };
    let mut ctrl = FloppyController::from_config(&cfg)?;
    let mut chip_ram = vec![0xAA, 0xAA, 0x55, 0x55];

    let selected = !CIAB_DSKMOTOR & !CIAB_DSKSEL0;
    ctrl.write_prb(selected);
    ctrl.tick(MOTOR_READY_CCK, 0, &mut chip_ram);
    ctrl.ensure_track(0, 0);
    ctrl.drives[0].set_rotation_word(0);
    ctrl.drives[0].rotation_acc_cck = 0;
    ctrl.set_dskpt_low(0);

    let len = DSKLEN_DMAEN | DSKLEN_WRITE | 2;
    assert!(!ctrl.write_dsklen(len, 0));
    assert!(!ctrl.write_dsklen(len, 0));
    let dmacon = DMACON_DMAEN | DMACON_DISK;
    let word_cck = FloppyController::word_cck_for_track_words(track0.len());
    assert!(!ctrl.tick(word_cck, dmacon, &mut chip_ram));

    let step_high = selected & !CIAB_DSKDIREC;
    ctrl.write_prb(step_high);
    ctrl.write_prb(step_high & !CIAB_DSKSTEP);
    assert_eq!(ctrl.track_for_drive(0), 2);

    assert!(ctrl.tick(word_cck, dmacon, &mut chip_ram));

    let mut reloaded = FloppyController::from_config(&cfg)?;
    reloaded.write_prb(selected);
    reloaded.tick(MOTOR_READY_CCK, 0, &mut []);
    reloaded.ensure_track(0, 0);
    assert_eq!(reloaded.drives[0].cached_words(), [0xAAAA, 0x0000]);
    reloaded.ensure_track(0, 2);
    assert_eq!(reloaded.drives[0].cached_words(), [0xFFFF, 0x5557]);

    let _ = fs::remove_file(path);
    Ok(())
}

#[test]
fn writable_legacy_extended_adf_raw_track_persists_word_stream() -> Result<()> {
    let path = temp_ext1_raw(&[0x4489, 0x1111, 0x2222])?;
    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: 100,
        drives: [
            Some(FloppyDriveConfig {
                path: path.clone(),
                write_protected: false,
            }),
            None,
            None,
            None,
        ],
    };
    let mut ctrl = FloppyController::from_config(&cfg)?;
    let words: [u16; 3] = [0x1234, 0x5678, 0x9ABC];
    let mut chip_ram = vec![0u8; words.len() * 2 + 2];
    for (i, word) in words.iter().copied().enumerate() {
        let [hi, lo] = word.to_be_bytes();
        chip_ram[i * 2] = hi;
        chip_ram[i * 2 + 1] = lo;
    }

    ctrl.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    ctrl.tick(MOTOR_READY_CCK, 0, &mut chip_ram);
    ctrl.ensure_track(0, 0);
    ctrl.drives[0].set_rotation_word(0);
    ctrl.drives[0].rotation_acc_cck = 0;
    ctrl.set_dskpt_low(0);
    let len = DSKLEN_DMAEN | DSKLEN_WRITE | words.len() as u16;
    assert!(!ctrl.write_dsklen(len, 0));
    assert!(!ctrl.write_dsklen(len, 0));
    let dmacon = DMACON_DMAEN | DMACON_DISK;
    while !ctrl.tick(ctrl.word_cck(), dmacon, &mut chip_ram) {}

    let persisted = fs::read(&path)?;
    assert_eq!(&persisted[0..8], UAE_EXT1_SIGNATURE);
    assert_eq!(u16::from_be_bytes([persisted[8], persisted[9]]), 0x1234);
    assert_eq!(u16::from_be_bytes([persisted[10], persisted[11]]), 4);
    let payload_off = 8 + 160 * 4;
    assert_eq!(
        &persisted[payload_off..payload_off + 4],
        &[0x56, 0x78, 0x9A, 0xBA]
    );

    let mut reloaded = FloppyController::from_config(&cfg)?;
    reloaded.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    reloaded.tick(MOTOR_READY_CCK, 0, &mut []);
    reloaded.ensure_track(0, 0);
    assert_eq!(reloaded.drives[0].cached_words(), [0x1234, 0x5678, 0x9ABA]);

    let _ = fs::remove_file(path);
    Ok(())
}

#[test]
fn writable_legacy_extended_adf_raw_track_overwrites_sync_boundary() -> Result<()> {
    let path = temp_ext1_raw(&[0x4489, 0x1111, 0x2222])?;
    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: 100,
        drives: [
            Some(FloppyDriveConfig {
                path: path.clone(),
                write_protected: false,
            }),
            None,
            None,
            None,
        ],
    };
    let mut ctrl = FloppyController::from_config(&cfg)?;
    let mut chip_ram = vec![0xAB, 0xCD];

    ctrl.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    ctrl.tick(MOTOR_READY_CCK, 0, &mut chip_ram);
    ctrl.ensure_track(0, 0);
    // Head at word 0, 8 cells in (bit 8) -- the write's landing bit phase.
    ctrl.drives[0].set_rotation_bit(8);
    let word_cck = FloppyController::word_cck_for_track_words(3);
    ctrl.set_dskpt_low(0);
    let len = DSKLEN_DMAEN | DSKLEN_WRITE | 1;
    assert!(!ctrl.write_dsklen(len, 0));
    assert!(!ctrl.write_dsklen(len, 0));
    let dmacon = DMACON_DMAEN | DMACON_DISK;
    while !ctrl.tick(word_cck, dmacon, &mut chip_ram) {}

    let persisted = fs::read(&path)?;
    assert_eq!(u16::from_be_bytes([persisted[8], persisted[9]]), 0x44AB);
    assert_eq!(u16::from_be_bytes([persisted[10], persisted[11]]), 4);
    let payload_off = 8 + 160 * 4;
    assert_eq!(
        &persisted[payload_off..payload_off + 4],
        &[0xC9, 0x11, 0x22, 0x22]
    );

    let mut reloaded = FloppyController::from_config(&cfg)?;
    reloaded.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    reloaded.tick(MOTOR_READY_CCK, 0, &mut []);
    reloaded.ensure_track(0, 0);
    assert_eq!(reloaded.drives[0].cached_words(), [0x44AB, 0xC911, 0x2222]);

    let _ = fs::remove_file(path);
    Ok(())
}

#[test]
fn writable_legacy_extended_adf_raw_track_preserves_odd_payload_length() -> Result<()> {
    let path = temp_ext1_raw_payload(0x4489, &[0x12, 0x34, 0xA0])?;
    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: 100,
        drives: [
            Some(FloppyDriveConfig {
                path: path.clone(),
                write_protected: false,
            }),
            None,
            None,
            None,
        ],
    };
    let mut ctrl = FloppyController::from_config(&cfg)?;
    let mut chip_ram = vec![0xFF, 0xFF];

    ctrl.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    ctrl.tick(MOTOR_READY_CCK, 0, &mut chip_ram);
    ctrl.ensure_track(0, 0);
    ctrl.drives[0].set_rotation_word(1);
    ctrl.drives[0].rotation_acc_cck = 0;
    ctrl.set_dskpt_low(0);
    let len = DSKLEN_DMAEN | DSKLEN_WRITE | 1;
    assert!(!ctrl.write_dsklen(len, 0));
    assert!(!ctrl.write_dsklen(len, 0));
    let dmacon = DMACON_DMAEN | DMACON_DISK;
    while !ctrl.tick(ctrl.word_cck(), dmacon, &mut chip_ram) {}

    let persisted = fs::read(&path)?;
    assert_eq!(u16::from_be_bytes([persisted[8], persisted[9]]), 0x4489);
    assert_eq!(u16::from_be_bytes([persisted[10], persisted[11]]), 3);
    let payload_off = 8 + 160 * 4;
    assert_eq!(
        &persisted[payload_off..payload_off + 3],
        &[0xFF, 0xFC, 0xA0]
    );

    let mut reloaded = FloppyController::from_config(&cfg)?;
    reloaded.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    reloaded.tick(MOTOR_READY_CCK, 0, &mut []);
    reloaded.ensure_track(0, 0);
    assert_eq!(reloaded.drives[0].cached_words(), [0x4489, 0xFFFC, 0xA000]);

    let _ = fs::remove_file(path);
    Ok(())
}

#[test]
fn write_dma_decodes_and_persists_track() -> Result<()> {
    let path = temp_adf()?;
    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: 100,
        drives: [
            Some(FloppyDriveConfig {
                path: path.clone(),
                write_protected: false,
            }),
            None,
            None,
            None,
        ],
    };
    let mut ctrl = FloppyController::from_config(&cfg)?;
    let mut source = vec![0u8; ADF_SIZE];
    source[0..BYTES_PER_SECTOR].fill(0xA5);
    let words = encode_adf_track(0, &source);
    let mut chip_ram = vec![0u8; words.len() * 2 + 2];
    for (i, word) in words.iter().copied().enumerate() {
        let [hi, lo] = word.to_be_bytes();
        chip_ram[i * 2] = hi;
        chip_ram[i * 2 + 1] = lo;
    }

    ctrl.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    ctrl.tick(MOTOR_READY_CCK, 0, &mut chip_ram);
    ctrl.set_dskpt_low(0);
    let len = DSKLEN_DMAEN | DSKLEN_WRITE | (words.len() as u16 & DSKLEN_MASK);
    assert!(!ctrl.write_dsklen(len, 0));
    assert!(!ctrl.write_dsklen(len, 0));
    let dmacon = DMACON_DMAEN | DMACON_DISK;
    while !ctrl.tick(ctrl.word_cck(), dmacon, &mut chip_ram) {}

    let persisted = fs::read(&path)?;
    assert_eq!(&persisted[0..BYTES_PER_SECTOR], &[0xA5; BYTES_PER_SECTOR]);
    let _ = fs::remove_file(path);
    Ok(())
}

#[test]
fn floppy_turbo_bursts_write_dma_and_persists_track() -> Result<()> {
    let path = temp_adf()?;
    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: SPEED_TURBO,
        drives: [
            Some(FloppyDriveConfig {
                path: path.clone(),
                write_protected: false,
            }),
            None,
            None,
            None,
        ],
    };
    let mut ctrl = FloppyController::from_config(&cfg)?;
    let mut source = vec![0u8; ADF_SIZE];
    source[0..BYTES_PER_SECTOR].fill(0x5C);
    let words = encode_adf_track(0, &source);
    let mut chip_ram = vec![0u8; words.len() * 2 + 2];
    for (i, word) in words.iter().copied().enumerate() {
        let [hi, lo] = word.to_be_bytes();
        chip_ram[i * 2] = hi;
        chip_ram[i * 2 + 1] = lo;
    }

    ctrl.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    ctrl.tick(MOTOR_READY_CCK, 0, &mut chip_ram);
    ctrl.set_dskpt_low(0);
    let len = DSKLEN_DMAEN | DSKLEN_WRITE | (words.len() as u16 & DSKLEN_MASK);
    assert!(!ctrl.write_dsklen(len, 0));
    assert!(!ctrl.write_dsklen(len, 0));
    let dmacon = DMACON_DMAEN | DMACON_DISK;
    // A full-track write takes a whole revolution at real pace; the
    // tick that crosses the grace window bursts it to completion.
    assert!(ctrl.tick(TURBO_DMA_GRACE_CCK, dmacon, &mut chip_ram));

    let persisted = fs::read(&path)?;
    assert_eq!(&persisted[0..BYTES_PER_SECTOR], &[0x5C; BYTES_PER_SECTOR]);
    let _ = fs::remove_file(path);
    Ok(())
}

#[test]
fn trackloader_sized_wordsync_window_decodes_full_amigados_track() -> Result<()> {
    let mut adf = vec![0u8; ADF_SIZE];
    let track = 35;
    for sector in 0..SECTORS_PER_TRACK {
        let off = adf_sector_offset(track, sector);
        for (idx, byte) in adf[off..off + BYTES_PER_SECTOR].iter_mut().enumerate() {
            *byte = (track as u8).wrapping_mul(3) ^ (sector as u8) ^ idx as u8;
        }
    }

    let words = encode_adf_track(track, &adf);
    let sync_positions = words
        .iter()
        .enumerate()
        .filter_map(|(idx, word)| (*word == DEFAULT_DSKSYNC).then_some(idx))
        .collect::<Vec<_>>();

    for sync_pos in sync_positions {
        let mut window = Vec::with_capacity(6400);
        window.push(0);
        for offset in 1..=6398 {
            window.push(words[(sync_pos + offset) % words.len()]);
        }
        window.push(DEFAULT_DSKSYNC);
        if window[1] != DEFAULT_DSKSYNC {
            window[0] = DEFAULT_DSKSYNC;
        }

        let decoded = decode_track_write(track, &window)
            .with_context(|| format!("decoding wordsync window after sync {sync_pos}"))?;
        assert_eq!(decoded.len(), SECTORS_PER_TRACK);
        for (sector, data) in decoded {
            let off = adf_sector_offset(track, sector);
            assert_eq!(&data[..], &adf[off..off + BYTES_PER_SECTOR]);
        }
    }

    Ok(())
}

#[test]
fn standard_adf_pal_revolution_leaves_index_gap_after_last_sector() -> Result<()> {
    let adf = vec![0u8; ADF_SIZE];
    let words = encode_adf_track(0, &adf);
    assert_eq!(words.len(), STANDARD_ADF_TRACK_WORDS);

    let mut sector_sync = [None; SECTORS_PER_TRACK];
    let mut pos = 0usize;
    while pos < words.len() {
        if words[pos] != DEFAULT_DSKSYNC {
            pos += 1;
            continue;
        }

        let sync_pos = pos;
        while pos < words.len() && words[pos] == DEFAULT_DSKSYNC {
            pos += 1;
        }
        let Some((info, _)) = decode_block(&words, pos, 4) else {
            continue;
        };
        if info[0] == 0xFF && info[1] == 0 && usize::from(info[2]) < SECTORS_PER_TRACK {
            sector_sync[usize::from(info[2])] = Some(sync_pos);
        }
    }

    let sector10_sync = sector_sync[10].context("sector 10 sync")?;
    let sector0_sync = sector_sync[0].context("sector 0 sync")?;
    let distance_to_sector0_second_sync =
        (sector0_sync + 1 + words.len() - sector10_sync) % words.len();

    // After decoding the sector whose AmigaDOS "sectors until gap" byte is
    // 1, some raw loaders skip a fixed 0x258-byte index gap before looking
    // for sector 0. The synthetic PAL track must leave enough physical gap
    // for that skip to land before sector 0's second sync word.
    let sector10_sync_to_even_data_end = AMIGADOS_SECTOR_MFM_WORDS - 2;
    let fixed_index_gap_skip_words = 0x258 / 2;
    let scan_restart_lead_words = 3;
    let fixed_skip_restart =
        sector10_sync_to_even_data_end + fixed_index_gap_skip_words + scan_restart_lead_words;

    assert!(
            distance_to_sector0_second_sync >= fixed_skip_restart,
            "sector 0 sync is {distance_to_sector0_second_sync} words after sector 10 sync; fixed post-gap skip restarts at {fixed_skip_restart}"
        );

    Ok(())
}

#[test]
fn non_wordsync_full_track_dma_uses_recovered_disk_word_phase() -> Result<()> {
    let mut adf = vec![0u8; ADF_SIZE];
    let boot = b"DOS\0";
    adf[0..boot.len()].copy_from_slice(boot);
    for (idx, byte) in adf[4..BYTES_PER_SECTOR].iter_mut().enumerate() {
        *byte = (idx as u8).wrapping_mul(3).wrapping_add(0x19);
    }
    let path = temp_path("full-track-dma-checksum.adf");
    fs::write(&path, &adf)?;
    let cfg = FloppyConfig {
        bridges: std::array::from_fn(|_| None),
        speed: 100,
        drives: [
            Some(FloppyDriveConfig {
                path: path.clone(),
                write_protected: true,
            }),
            None,
            None,
            None,
        ],
    };
    let mut ctrl = FloppyController::from_config(&cfg)?;
    let mut chip_ram = vec![0u8; 7358 * 2];
    ctrl.write_prb(!CIAB_DSKMOTOR & !CIAB_DSKSEL0);
    ctrl.tick(MOTOR_READY_CCK, 0, &mut chip_ram);
    ctrl.ensure_track(0, 0);
    // Arm just before a natural 16-bit MFM word boundary. Trackdisk-style
    // full-track reads do not always use WORDSYNC, so DMA must still drain
    // Paula's recovered word phase rather than starting a new phase on the
    // CPU's DSKLEN write.
    ctrl.drives[0].set_rotation_bit(3904);
    ctrl.drives[0].rotation_acc_cck = 0;
    let lead_in = ctrl.drives[0].head_cck_for_bits(15) as u32;
    ctrl.tick(lead_in, 0, &mut chip_ram);
    assert_eq!(ctrl.drives[0].rotation_bit % 16, 15);
    ctrl.set_dskpt_low(0);

    let len = DSKLEN_DMAEN | 7358;
    assert!(!ctrl.write_dsklen(len, 0));
    assert!(!ctrl.write_dsklen(len, 0));
    let dmacon = DMACON_DMAEN | DMACON_DISK;
    while !ctrl.tick(ctrl.word_cck(), dmacon, &mut chip_ram) {}

    let sectors = decode_track_write(0, &bytes_to_words(&chip_ram))?;
    let sector0 = sectors.iter().find(|(sector, _)| *sector == 0);
    let Some((_, sector0)) = sector0 else {
        panic!(
            "full-track DMA should include sector 0; decoded sectors: {:?}",
            sectors
                .iter()
                .map(|(sector, _)| *sector)
                .collect::<Vec<_>>()
        );
    };
    assert_eq!(&sector0[..], &adf[0..BYTES_PER_SECTOR]);

    let _ = fs::remove_file(path);
    Ok(())
}

fn temp_adf() -> Result<PathBuf> {
    let path = temp_path("test.adf");
    fs::write(&path, vec![0u8; ADF_SIZE])?;
    Ok(path)
}

#[test]
fn drive_connected_and_disk_inserted_track_drive_state() -> Result<()> {
    let mut ctrl = FloppyController::default();
    // DF0 is the internal drive: always connected, starts empty.
    assert!(ctrl.drive_connected(0));
    assert!(!ctrl.disk_inserted(0));
    // DF1-DF3 are not wired up by default.
    assert!(!ctrl.drive_connected(1));
    assert!(!ctrl.drive_connected(3));
    assert!(!ctrl.drive_connected(4));

    let adf = temp_adf()?;
    ctrl.insert_disk_image(0, adf.clone(), true)?;
    assert!(ctrl.disk_inserted(0));
    ctrl.eject_disk_image(0)?;
    assert!(!ctrl.disk_inserted(0));

    // A configured external drive answers the ID protocol and shows
    // as connected.
    let mut drives: [Option<FloppyDriveConfig>; 4] = std::array::from_fn(|_| None);
    drives[1] = Some(FloppyDriveConfig {
        path: adf.clone(),
        write_protected: true,
    });
    let ctrl = FloppyController::from_config(&FloppyConfig {
        drives,
        bridges: std::array::from_fn(|_| None),
        speed: 100,
    })?;
    assert!(ctrl.drive_connected(1));
    assert!(ctrl.disk_inserted(1));
    let _ = fs::remove_file(&adf);
    Ok(())
}

fn temp_adz(adf: &[u8]) -> Result<PathBuf> {
    temp_gzip("test.adz", adf)
}

/// One gzip member holding `data`. Concatenating two of these makes the
/// multi-member stream `MultiGzDecoder` exists for.
fn gzip_bytes(data: &[u8]) -> Result<Vec<u8>> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(data)?;
    Ok(encoder.finish()?)
}

fn temp_gzip(name: &str, data: &[u8]) -> Result<PathBuf> {
    let path = temp_path(name);
    fs::write(&path, gzip_bytes(data)?)?;
    Ok(path)
}

fn temp_ext2_raw(words: &[u16]) -> Result<PathBuf> {
    let payload: Vec<u8> = words.iter().flat_map(|word| word.to_be_bytes()).collect();
    temp_ext2_track(1, (payload.len() * 8) as u32, &payload)
}

fn temp_ext2_raw_tracks(raw_tracks: &[(usize, &[u16])]) -> Result<PathBuf> {
    let path = temp_path("test.ext.adf");
    let track_count = raw_tracks
        .iter()
        .map(|(track, _)| track + 1)
        .max()
        .unwrap_or(0);
    ensure!(track_count > 0, "raw track map must not be empty");
    let mut tracks = vec![None; track_count];
    for &(track, words) in raw_tracks {
        ensure!(track < track_count, "raw track index is outside track map");
        tracks[track] = Some(words);
    }

    let mut image = Vec::new();
    let mut payloads = Vec::new();
    image.extend_from_slice(UAE_EXT2_SIGNATURE);
    image.extend_from_slice(&0u16.to_be_bytes());
    image.extend_from_slice(&(track_count as u16).to_be_bytes());
    for track in tracks {
        match track {
            Some(words) => {
                let payload: Vec<u8> = words.iter().copied().flat_map(u16::to_be_bytes).collect();
                image.extend_from_slice(&0u16.to_be_bytes());
                image.push(0);
                image.push(1);
                image.extend_from_slice(&(payload.len() as u32).to_be_bytes());
                image.extend_from_slice(&((payload.len() * 8) as u32).to_be_bytes());
                payloads.extend_from_slice(&payload);
            }
            None => {
                image.extend_from_slice(&[0; 12]);
            }
        }
    }
    image.extend_from_slice(&payloads);
    fs::write(&path, image)?;
    Ok(path)
}

fn temp_ext1_raw(words: &[u16]) -> Result<PathBuf> {
    let sync = words.first().copied().unwrap_or(DEFAULT_DSKSYNC);
    let payload: Vec<u8> = words
        .iter()
        .copied()
        .skip(1)
        .flat_map(u16::to_be_bytes)
        .collect();
    temp_ext1_raw_payload(sync, &payload)
}

fn temp_ext1_raw_payload(sync: u16, payload: &[u8]) -> Result<PathBuf> {
    let path = temp_path("test-legacy.ext.adf");
    let mut image = Vec::new();
    image.extend_from_slice(UAE_EXT1_SIGNATURE);
    image.extend_from_slice(&sync.to_be_bytes());
    image.extend_from_slice(&(payload.len() as u16).to_be_bytes());
    for _ in 1..160 {
        image.extend_from_slice(&0u16.to_be_bytes());
        image.extend_from_slice(&0u16.to_be_bytes());
    }
    image.extend_from_slice(payload);
    fs::write(&path, image)?;
    Ok(path)
}

fn temp_ext2_raw_revolutions(words: &[u16], bit_len: u32, revolutions: u8) -> Result<PathBuf> {
    let payload: Vec<u8> = words.iter().copied().flat_map(u16::to_be_bytes).collect();
    temp_ext2_track_with_revolutions(1, bit_len, revolutions, &payload)
}

fn temp_ext2_amigados(track_data: &[u8]) -> Result<PathBuf> {
    temp_ext2_track(0, (track_data.len() * 8) as u32, track_data)
}

fn temp_ext2_amigados_plus_raw(
    track_data: &[u8],
    raw_words: &[u16],
    raw_bit_len: u32,
    raw_revolutions: u8,
) -> Result<PathBuf> {
    let raw_payload: Vec<u8> = raw_words
        .iter()
        .copied()
        .flat_map(u16::to_be_bytes)
        .collect();
    let path = temp_path("test-mixed.ext.adf");
    let mut image = Vec::new();
    image.extend_from_slice(UAE_EXT2_SIGNATURE);
    image.extend_from_slice(&0u16.to_be_bytes());
    image.extend_from_slice(&2u16.to_be_bytes());

    image.extend_from_slice(&0u16.to_be_bytes());
    image.push(0);
    image.push(0);
    image.extend_from_slice(&(track_data.len() as u32).to_be_bytes());
    image.extend_from_slice(&((track_data.len() * 8) as u32).to_be_bytes());

    image.extend_from_slice(&0u16.to_be_bytes());
    image.push(raw_revolutions.saturating_sub(1));
    image.push(1);
    image.extend_from_slice(&(raw_payload.len() as u32).to_be_bytes());
    image.extend_from_slice(&raw_bit_len.to_be_bytes());

    image.extend_from_slice(track_data);
    image.extend_from_slice(&raw_payload);
    fs::write(&path, image)?;
    Ok(path)
}

fn temp_ext2_track(track_type: u8, bit_len: u32, payload: &[u8]) -> Result<PathBuf> {
    temp_ext2_track_with_revolutions(track_type, bit_len, 1, payload)
}

fn temp_ext2_track_with_revolutions(
    track_type: u8,
    bit_len: u32,
    revolutions: u8,
    payload: &[u8],
) -> Result<PathBuf> {
    let path = temp_path("test.ext.adf");
    fs::write(
        &path,
        ext2_track_image(track_type, bit_len, revolutions, payload),
    )?;
    Ok(path)
}

fn ext2_track_image(track_type: u8, bit_len: u32, revolutions: u8, payload: &[u8]) -> Vec<u8> {
    let mut image = Vec::new();
    image.extend_from_slice(UAE_EXT2_SIGNATURE);
    image.extend_from_slice(&0u16.to_be_bytes());
    image.extend_from_slice(&1u16.to_be_bytes());
    image.extend_from_slice(&0u16.to_be_bytes());
    image.push(revolutions.saturating_sub(1));
    image.push(track_type);
    image.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    image.extend_from_slice(&bit_len.to_be_bytes());
    image.extend_from_slice(payload);
    image
}

fn temp_scp_raw_revolutions(revolutions: &[&[u16]], bit_len: u32) -> Result<PathBuf> {
    temp_scp_raw_revolutions_with_flags(revolutions, bit_len, SCP_FLAG_INDEX)
}

fn temp_scp_raw_revolutions_with_flags(
    revolutions: &[&[u16]],
    bit_len: u32,
    flags: u8,
) -> Result<PathBuf> {
    let rev_count = revolutions.len();
    let mut flux_payloads = Vec::with_capacity(rev_count);
    for words in revolutions {
        flux_payloads.push(scp_flux_entries_for_words(words, bit_len));
    }
    temp_scp_flux_payloads(flux_payloads, bit_len, flags)
}

fn temp_scp_flux_entries(entries: &[u16], bit_len: u32) -> Result<PathBuf> {
    let mut payload = Vec::with_capacity(entries.len() * 2);
    for entry in entries {
        payload.extend_from_slice(&entry.to_be_bytes());
    }
    temp_scp_flux_payloads(vec![payload], bit_len, SCP_FLAG_INDEX)
}

fn temp_scp_flux_payloads(flux_payloads: Vec<Vec<u8>>, bit_len: u32, flags: u8) -> Result<PathBuf> {
    let path = temp_path("test.scp");
    let rev_count = flux_payloads.len();
    ensure!(rev_count > 0 && rev_count <= u8::MAX as usize);
    let track_table_offset = scp_track_table_offset(flags);
    let tdh_offset = track_table_offset + SCP_TRACK_TABLE_LEN;
    let flux_offset = 4 + rev_count * 12;
    let index_time = bit_len * (AMIGA_DD_BITCELL_NS / SCP_CAPTURE_BASE_NS) as u32;

    let mut image = vec![0; tdh_offset];
    image[0..3].copy_from_slice(SCP_SIGNATURE);
    image[0x03] = 0x25;
    image[0x04] = 0x04;
    image[0x05] = rev_count as u8;
    image[0x06] = 0;
    image[0x07] = 0;
    image[0x08] = flags;
    image[0x09] = SCP_DEFAULT_16_BIT_FLUX_WIDTH;
    image[0x0A] = 0;
    image[0x0B] = 0;
    image[track_table_offset..track_table_offset + 4]
        .copy_from_slice(&(tdh_offset as u32).to_le_bytes());

    let mut track = Vec::new();
    track.extend_from_slice(b"TRK");
    track.push(0);
    let mut data_offset = flux_offset;
    for flux in &flux_payloads {
        track.extend_from_slice(&index_time.to_le_bytes());
        track.extend_from_slice(&((flux.len() / 2) as u32).to_le_bytes());
        track.extend_from_slice(&(data_offset as u32).to_le_bytes());
        data_offset += flux.len();
    }
    for flux in flux_payloads {
        track.extend_from_slice(&flux);
    }
    image.extend_from_slice(&track);
    fs::write(&path, image)?;
    Ok(path)
}

fn write_scp_checksum(image: &mut [u8]) {
    let checksum = scp_checksum(image);
    image[SCP_CHECKSUM_OFFSET..SCP_CHECKSUM_OFFSET + 4].copy_from_slice(&checksum.to_le_bytes());
}

fn scp_flux_entries_for_words(words: &[u16], bit_len: u32) -> Vec<u8> {
    let ticks_per_cell = (AMIGA_DD_BITCELL_NS / SCP_CAPTURE_BASE_NS) as u16;
    let mut flux = Vec::new();
    let mut previous_transition_end = 0u32;
    for bit_idx in 0..bit_len {
        let word = words.get((bit_idx / 16) as usize).copied().unwrap_or(0);
        let bit_pos = 15 - (bit_idx % 16);
        if word & (1 << bit_pos) == 0 {
            continue;
        }
        let cells = bit_idx + 1 - previous_transition_end;
        let ticks = (cells as u16) * ticks_per_cell;
        flux.extend_from_slice(&ticks.to_be_bytes());
        previous_transition_end = bit_idx + 1;
    }
    flux
}

fn temp_path(name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("copperline-floppy-test-{nanos}-{counter}-{name}"))
}
