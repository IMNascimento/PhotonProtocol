//! The phase 1 exit criterion: a file survives the whole pipeline through a
//! distorted channel.
//!
//! Every other test in this crate exercises one layer against its neighbours.
//! These run the actual thing — compress, fountain-code, frame, paint, distort,
//! sample, classify, repair, reassemble, decompress, verify — and require the
//! bytes that come out to be the bytes that went in.
//!
//! The receiver is handed pictures and finds the frame itself, exactly as it
//! will have to on a real recording. One test deliberately supplies the
//! channel's own transform instead, so that a failure to *find* a frame can
//! still be told apart from a failure to *read* one — from the outside the two
//! look identical.

use photon_core::error::Error;
use photon_core::profile::ProfileId;
use photon_core::session::{FrameOutcome, Receiver, Transmitter};
use photon_core::simulate::Channel;

/// Text-like input, so compression is exercised rather than skipped.
fn document(len: usize) -> Vec<u8> {
    let source = "PhotonProtocol moves a file across a channel that never answers back. \
                  The emitter paints, the camera films, and nothing is ever sent twice. ";
    source.bytes().cycle().take(len).collect()
}

/// Input that will not compress, so the transfer really has to carry every byte.
fn opaque(len: usize) -> Vec<u8> {
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            u8::try_from(state & 0xFF).unwrap_or(0)
        })
        .collect()
}

/// Whether the receiver locates the frame itself or is told where it is.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Locate {
    /// The receiver runs the detector, as it will on a real recording.
    ByDetection,
    /// The receiver is handed the channel's own transform, isolating everything
    /// above detection.
    FromGroundTruth,
}

/// Runs a transfer through a channel and reports how many frames it took.
fn transfer(
    name: &str,
    file: &[u8],
    profile: ProfileId,
    channel: &Channel,
    cell_px: u32,
    frame_limit: usize,
    locate: Locate,
) -> Result<(Vec<u8>, usize, usize), Error> {
    let mut tx = Transmitter::new(name, file, profile, 0x0BAD_C0DE).expect("transfer prepared");
    let mut rx = Receiver::new(profile);

    let mut frames = 0usize;
    let mut lost = 0usize;

    while !rx.is_complete() && frames < frame_limit {
        let frame = tx.next_frame(cell_px).expect("frame painted");
        let source_transform = frame_transform(profile, cell_px);
        let capture = channel.apply(&frame.image, &source_transform);

        let report = match locate {
            Locate::ByDetection => rx.accept_image(&capture.image),
            Locate::FromGroundTruth => rx.accept_frame(&capture.image, &capture.transform),
        };
        if report.outcome != FrameOutcome::Decoded {
            lost += 1;
        }
        frames += 1;
    }

    rx.finish().map(|received| {
        assert_eq!(received.name, name);
        (received.bytes, frames, lost)
    })
}

fn frame_transform(profile: ProfileId, cell_px: u32) -> photon_core::Homography {
    photon_core::FrameLayout::new(profile.profile()).identity_transform(cell_px)
}

#[test]
fn a_document_survives_a_distorted_channel_the_receiver_locates_itself() {
    // The whole pipeline, with nothing handed over: the receiver gets pictures
    // and has to find the code in them.
    let file = document(24_000);
    let channel = Channel::severity(0.3);

    let (received, frames, lost) =
        transfer("notes.txt", &file, ProfileId::P2Standard, &channel, 8, 200, Locate::ByDetection)
            .expect("the transfer completed");

    assert_eq!(received, file, "the reconstructed file differs from the original");
    assert!(frames > 0);
    assert_eq!(lost, 0, "no frame should be lost at severity 0.3");
}

#[test]
fn detection_costs_nothing_a_known_transform_would_have_saved() {
    // Detection recovers the transform from the image alone. If it were even a
    // fraction of a cell out, frames would start failing that succeed when the
    // position is known, so the two paths are compared directly.
    let file = opaque(20_000);
    let channel = Channel::severity(0.3);

    let (by_truth, truth_frames, truth_lost) = transfer(
        "blob.bin",
        &file,
        ProfileId::P2Standard,
        &channel,
        8,
        300,
        Locate::FromGroundTruth,
    )
    .expect("the transfer completed with a known transform");

    let (by_detection, detected_frames, detected_lost) =
        transfer("blob.bin", &file, ProfileId::P2Standard, &channel, 8, 300, Locate::ByDetection)
            .expect("the transfer completed by detection");

    assert_eq!(by_truth, file);
    assert_eq!(by_detection, file);
    assert_eq!(truth_lost, detected_lost, "detection lost frames a known transform did not");
    assert_eq!(truth_frames, detected_frames, "detection needed more frames");
}

#[test]
fn an_incompressible_file_survives_a_distorted_channel() {
    // Text compresses so hard that a "large" document can fit in two frames.
    // Opaque bytes force the transport layer to actually carry the file.
    let file = opaque(20_000);
    let channel = Channel::severity(0.25);

    let (received, frames, _) =
        transfer("blob.bin", &file, ProfileId::P2Standard, &channel, 8, 300, Locate::ByDetection)
            .expect("the transfer completed");

    assert_eq!(received, file);
    assert!(frames > 1, "an incompressible file should need more than one frame");
}

#[test]
fn every_profile_completes_a_transfer_through_the_channel() {
    let file = document(12_000);
    for profile in [ProfileId::P1Conservative, ProfileId::P2Standard, ProfileId::P3Dense] {
        let channel = Channel::severity(0.2);
        let (received, _, lost) =
            transfer("shared.txt", &file, profile, &channel, 8, 300, Locate::ByDetection)
                .unwrap_or_else(|e| panic!("{:?} failed: {e}", profile.profile().name));

        assert_eq!(received, file, "{} reconstructed the wrong bytes", profile.profile().name);
        assert_eq!(lost, 0, "{} lost a frame at severity 0.2", profile.profile().name);
    }
}

#[test]
fn a_recording_that_starts_before_the_screen_is_in_shot_is_survivable() {
    // Nobody points the camera first and presses record second. The leading
    // frames of a real recording contain a table, a hand, or a ceiling, and the
    // receiver has to shrug them off rather than fail.
    use photon_core::Rgb;
    use photon_core::RgbImage;

    let file = document(9_000);
    let channel = Channel::severity(0.2);
    let profile = ProfileId::P2Standard;

    let mut tx = Transmitter::new("late.txt", &file, profile, 11).expect("prepared");
    let mut rx = Receiver::new(profile);

    for shade in [20u8, 90, 200] {
        let junk = RgbImage::filled(600, 400, Rgb::new(shade, shade, shade));
        let report = rx.accept_image(&junk);
        assert_eq!(report.outcome, FrameOutcome::NotLocated, "junk was read as a frame");
    }

    let mut frames = 0usize;
    while !rx.is_complete() && frames < 200 {
        let frame = tx.next_frame(8).expect("painted");
        let capture = channel.apply(&frame.image, &frame_transform(profile, 8));
        rx.accept_image(&capture.image);
        frames += 1;
    }

    assert!(rx.is_complete(), "the transfer never completed after the junk");
    assert_eq!(rx.finish().expect("completed").bytes, file);
}

#[test]
fn dropped_frames_cost_time_and_not_correctness() {
    // The property the entire design rests on. Half the frames are thrown away
    // before the receiver ever sees them -- as a hand moving, a reflection, or
    // a camera that simply missed one would do -- and the file still arrives.
    let file = document(16_000);
    let channel = Channel::severity(0.25);
    let profile = ProfileId::P2Standard;

    let mut tx = Transmitter::new("lossy.txt", &file, profile, 42).expect("prepared");
    let mut rx = Receiver::new(profile);
    let source_transform = frame_transform(profile, 8);

    let mut frames = 0usize;
    let mut delivered = 0usize;
    while !rx.is_complete() && frames < 300 {
        let frame = tx.next_frame(8).expect("painted");
        frames += 1;
        if frames.is_multiple_of(2) {
            continue;
        }
        let capture = channel.apply(&frame.image, &source_transform);
        rx.accept_image(&capture.image);
        delivered += 1;
    }

    assert!(rx.is_complete(), "half-rate delivery failed after {delivered} frames");
    assert_eq!(rx.finish().expect("completed").bytes, file);
}

#[test]
fn a_transfer_that_ran_out_of_frames_reports_the_shortfall() {
    // SPEC.md 9.2: a decoder must never report a bare failure. This is the
    // path a user actually hits -- they stopped filming too early -- and the
    // difference between "nearly there" and "start again" is the whole message.
    let file = opaque(80_000);
    let channel = Channel::severity(0.25);

    match transfer(
        "truncated.bin",
        &file,
        ProfileId::P2Standard,
        &channel,
        8,
        3,
        Locate::ByDetection,
    ) {
        Err(Error::InsufficientSymbols { accepted, needed }) => {
            assert!(accepted > 0, "three frames carried nothing");
            assert!(needed > accepted, "needed {needed}, accepted {accepted}");
        }
        Err(other) => panic!("expected a shortfall report, got {other}"),
        Ok((bytes, _, _)) => panic!("three frames should not carry {} bytes", bytes.len()),
    }
}
