//! WebAssembly bindings for PhotonProtocol.
//!
//! This crate is an adapter and holds no protocol logic of its own. Everything
//! it exposes delegates to `photon-core`, so the emitter page, the decoder page
//! and the native command line all run the same implementation and cannot drift
//! apart. Anything that belongs to the format belongs in `photon-core`; if a
//! change here would alter what goes on the wire, it is in the wrong crate.
//!
//! The boundary is deliberately narrow. Values crossing it are plain strings and
//! byte buffers rather than mirrored object graphs, because every mirrored type
//! is a second definition of something the specification already defines once.

use photon_core::{PROTOCOL_VERSION, SPEC_VERSION, profile::PROFILES};
use wasm_bindgen::prelude::wasm_bindgen;

/// The protocol version this build implements (`SPEC.md` §5.1).
#[wasm_bindgen(js_name = protocolVersion)]
#[must_use]
pub fn protocol_version() -> u8 {
    PROTOCOL_VERSION
}

/// The version of the specification document this build was written against.
#[wasm_bindgen(js_name = specVersion)]
#[must_use]
pub fn spec_version() -> String {
    SPEC_VERSION.to_owned()
}

/// The physical-layer profiles and their derived geometry, as JSON.
///
/// The pages need this to populate a profile picker and to size a canvas. It is
/// serialised by hand rather than through a mirrored type: the geometry is
/// derived from the specification's formulas in one place, and a second
/// declaration of the same fields would only be somewhere for them to disagree.
#[wasm_bindgen(js_name = profiles)]
#[must_use]
pub fn profiles() -> String {
    let entries: Vec<String> = PROFILES
        .iter()
        .map(|p| {
            format!(
                concat!(
                    r#"{{"id":{},"name":"{}","grid":{},"shapes":{},"colours":{},"#,
                    r#""bitsPerCell":{},"dataCells":{},"payloadCapacity":{},"#,
                    r#""parityRate":{:.4},"linkOverhead":{:.4}}}"#
                ),
                p.id.as_u8(),
                p.name,
                p.grid,
                p.num_shapes,
                p.num_colours,
                p.bits_per_cell(),
                p.data_cells(),
                p.payload_capacity(),
                p.rs_parity_rate(),
                p.link_overhead(),
            )
        })
        .collect();

    format!("[{}]", entries.join(","))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles_json_is_well_formed() {
        // No JSON parser is linked in, so assert the shape structurally: the
        // page consumes this with JSON.parse and a stray quote would only
        // surface in a browser.
        let json = profiles();
        assert!(json.starts_with('[') && json.ends_with(']'), "{json}");
        assert_eq!(json.matches("\"id\":").count(), PROFILES.len());
        assert_eq!(json.matches('{').count(), json.matches('}').count());
        assert_eq!(json.matches('"').count() % 2, 0, "unbalanced quotes in {json}");
        assert!(!json.contains(",,") && !json.contains("[,") && !json.contains(",]"), "{json}");
    }

    #[test]
    fn bindings_report_the_core_versions() {
        assert_eq!(protocol_version(), PROTOCOL_VERSION);
        assert_eq!(spec_version(), SPEC_VERSION);
    }
}
