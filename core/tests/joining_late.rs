//! The way a person actually uses this: point a camera at a screen already
//! sending, and wait.
//!
//! These are regression tests for a real failure. Someone set the sending page
//! to `P1-conservative`, left the receiving page on its default, pointed a
//! camera, and waited through five full passes for a transfer that could never
//! have completed — then got told "no readable manifest was captured", which
//! explained none of it.
//!
//! Three separate faults combined to produce that. Each has its own test here,
//! because each could return on its own.

use photon_core::error::Error;
use photon_core::profile::{PROFILES, ProfileId};
use photon_core::session::{FrameOutcome, Receiver, Transmitter};

fn file(len: usize) -> Vec<u8> {
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

#[test]
fn a_receiver_told_nothing_reads_any_profile() {
    // The fault: the receiver was built for one profile and could not see any
    // other, so choosing a different one on the sending device made every frame
    // invisible. Nothing in either interface said they had to match.
    for profile in &PROFILES {
        let source = file(9_000);
        let mut tx = Transmitter::new("blob.bin", &source, profile.id, 0x51DE).expect("prepared");
        let mut rx = Receiver::new();

        let mut frames = 0usize;
        while !rx.is_complete() && frames < 300 {
            let frame = tx.next_frame(8).expect("painted");
            rx.accept_image(&frame.image);
            frames += 1;
        }

        assert!(rx.is_complete(), "{} never completed", profile.name);
        assert_eq!(rx.profile(), Some(profile.id), "{} was misidentified", profile.name);
        assert_eq!(rx.finish().expect("finished").bytes, source, "{}", profile.name);
    }
}

#[test]
fn joining_halfway_through_completes_on_the_next_pass() {
    // What the person was doing, and what should always have worked: start
    // watching partway through, keep watching, and let the stream come round.
    let source = file(30_000);
    let profile = ProfileId::P1Conservative;

    let mut tx = Transmitter::new("late.bin", &source, profile, 0x10E).expect("prepared");
    let mut rx = Receiver::new();

    // Miss the first third of the pass entirely.
    let skip = tx.frames_per_pass() / 3;
    for _ in 0..skip {
        tx.next_frame(8).expect("painted");
    }

    let mut frames = 0usize;
    while !rx.is_complete() && frames < 400 {
        let frame = tx.next_frame(8).expect("painted");
        rx.accept_image(&frame.image);
        frames += 1;
    }

    assert!(rx.is_complete(), "joining late never completed after {frames} frames");
    assert_eq!(rx.finish().expect("finished").bytes, source);
}

#[test]
fn seeing_only_a_fraction_of_the_frames_still_completes() {
    // A camera skips frames, a decoder busy with the last one skips more. The
    // manifest used to be sent once every eight frames on this profile, so a
    // receiver seeing one frame in four could miss it for a very long time
    // while discarding every symbol it did read.
    let source = file(30_000);
    let profile = ProfileId::P1Conservative;

    let mut tx = Transmitter::new("sparse.bin", &source, profile, 0x5A5).expect("prepared");
    let mut rx = Receiver::new();

    let mut shown = 0usize;
    let mut delivered = 0usize;
    while !rx.is_complete() && shown < 600 {
        let frame = tx.next_frame(8).expect("painted");
        shown += 1;
        if !shown.is_multiple_of(4) {
            continue;
        }
        rx.accept_image(&frame.image);
        delivered += 1;
    }

    assert!(rx.is_complete(), "one frame in four never completed after {delivered} of {shown}");
    assert_eq!(rx.finish().expect("finished").bytes, source);
}

#[test]
fn a_frame_spliced_from_two_codes_is_named_as_one() {
    // What a phone filming a monitor produces most of the time: the shutter
    // opens across a screen refresh, so the top of the picture is one code and
    // the bottom is the next. The payload is a splice and cannot be repaired --
    // but the two header copies sit at opposite edges and disagree, which says
    // precisely what happened and that the fix is on the sending device.
    //
    // Before this was recognised the symptom was frames located in their
    // hundreds, none of them used, and nothing on screen to say why.
    use photon_core::{Rgb, RgbImage};

    let source = file(30_000);
    let profile = ProfileId::P2Standard;
    let mut tx = Transmitter::new("torn.bin", &source, profile, 0x7EA).expect("prepared");

    let first = tx.next_frame(8).expect("painted");
    let second = tx.next_frame(8).expect("painted");
    assert_eq!(first.image.width(), second.image.width());

    // Take the top half from one code and the bottom half from the next.
    let side = first.image.width();
    let mut spliced = RgbImage::filled(side, side, Rgb::BLACK);
    for y in 0..side {
        let source_image = if y < side / 2 { &first.image } else { &second.image };
        for x in 0..side {
            spliced.set(x, y, source_image.get(x, y));
        }
    }

    let mut rx = Receiver::new();
    let report = rx.accept_image(&spliced);

    assert_eq!(
        report.outcome,
        FrameOutcome::Straddled,
        "a spliced frame must be recognised, not silently discarded"
    );
    assert_eq!(report.new_symbols, 0, "nothing from a spliced frame may be believed");

    // And an unspliced frame from the same sender still reads normally.
    let clean = tx.next_frame(8).expect("painted");
    assert_eq!(rx.accept_image(&clean.image).outcome, FrameOutcome::Decoded);
}

#[test]
fn a_failure_says_which_stage_gave_up() {
    // The fault that made the other two impossible to diagnose: every one of
    // these used to report "no readable manifest was captured", which describes
    // only the last of them.
    use photon_core::{Rgb, RgbImage};

    // Nothing in shot at all.
    let mut rx = Receiver::new();
    for shade in [10u8, 128, 240] {
        let blank = RgbImage::filled(500, 400, Rgb::new(shade, shade, shade));
        assert_eq!(rx.accept_image(&blank).outcome, FrameOutcome::NotLocated);
    }
    assert_eq!(rx.finish().unwrap_err(), Error::NoFinders);

    // Frames seen, but stopped before there was enough.
    let source = file(120_000);
    let mut tx = Transmitter::new("cut.bin", &source, ProfileId::P2Standard, 7).expect("prepared");
    let mut rx = Receiver::new();
    for _ in 0..2 {
        let frame = tx.next_frame(8).expect("painted");
        rx.accept_image(&frame.image);
    }

    match rx.finish() {
        Err(Error::InsufficientSymbols { accepted, needed }) => {
            assert!(accepted > 0 && needed > accepted, "{accepted} of {needed}");
        }
        other => panic!("expected a shortfall, got {other:?}"),
    }
}
