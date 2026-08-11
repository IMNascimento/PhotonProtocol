//! The phase 1 exit criterion: a file survives the whole pipeline through a
//! distorted channel.
//!
//! Every other test in this crate exercises one layer against its neighbours.
//! These run the actual thing — compress, fountain-code, frame, paint, distort,
//! sample, classify, repair, reassemble, decompress, verify — and require the
//! bytes that come out to be the bytes that went in.
//!
//! The channel is given to the receiver together with the transform it produced.
//! Detection is not being tested here; that is deliberate, because a failure to
//! *find* the frame and a failure to *read* it look identical from the outside
//! and need to be told apart.

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

/// Runs a transfer through a channel and reports how many frames it took.
fn transfer(
    name: &str,
    file: &[u8],
    profile: ProfileId,
    channel: &Channel,
    cell_px: u32,
    frame_limit: usize,
) -> Result<(Vec<u8>, usize, usize), Error> {
    let mut tx = Transmitter::new(name, file, profile, 0x0BAD_C0DE).expect("transfer prepared");
    let mut rx = Receiver::new(profile);

    let mut frames = 0usize;
    let mut lost = 0usize;

    while !rx.is_complete() && frames < frame_limit {
        let frame = tx.next_frame(cell_px).expect("frame painted");
        let source_transform = frame_transform(profile, cell_px);
        let capture = channel.apply(&frame.image, &source_transform);

        let report = rx.accept_frame(&capture.image, &capture.transform);
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
fn a_document_survives_a_distorted_channel() {
    let file = document(24_000);
    let channel = Channel::severity(0.3);

    let (received, frames, lost) =
        transfer("notes.txt", &file, ProfileId::P2Standard, &channel, 8, 200)
            .expect("the transfer completed");

    assert_eq!(received, file, "the reconstructed file differs from the original");
    assert!(frames > 0);
    assert_eq!(lost, 0, "no frame should be lost at severity 0.3");
}

#[test]
fn an_incompressible_file_survives_a_distorted_channel() {
    // Text compresses so hard that a "large" document can fit in two frames.
    // Opaque bytes force the transport layer to actually carry the file.
    let file = opaque(20_000);
    let channel = Channel::severity(0.25);

    let (received, frames, _) =
        transfer("blob.bin", &file, ProfileId::P2Standard, &channel, 8, 300)
            .expect("the transfer completed");

    assert_eq!(received, file);
    assert!(frames > 1, "an incompressible file should need more than one frame");
}

#[test]
fn every_profile_completes_a_transfer_through_the_channel() {
    let file = document(12_000);
    for profile in [ProfileId::P1Conservative, ProfileId::P2Standard, ProfileId::P3Dense] {
        let channel = Channel::severity(0.2);
        let (received, _, lost) = transfer("shared.txt", &file, profile, &channel, 8, 300)
            .unwrap_or_else(|e| panic!("{:?} failed: {e}", profile.profile().name));

        assert_eq!(received, file, "{} reconstructed the wrong bytes", profile.profile().name);
        assert_eq!(lost, 0, "{} lost a frame at severity 0.2", profile.profile().name);
    }
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
        if frames % 2 == 0 {
            continue;
        }
        let capture = channel.apply(&frame.image, &source_transform);
        rx.accept_frame(&capture.image, &capture.transform);
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

    match transfer("truncated.bin", &file, ProfileId::P2Standard, &channel, 8, 3) {
        Err(Error::InsufficientSymbols { accepted, needed }) => {
            assert!(accepted > 0, "three frames carried nothing");
            assert!(needed > accepted, "needed {needed}, accepted {accepted}");
        }
        Err(other) => panic!("expected a shortfall report, got {other}"),
        Ok((bytes, _, _)) => panic!("three frames should not carry {} bytes", bytes.len()),
    }
}
